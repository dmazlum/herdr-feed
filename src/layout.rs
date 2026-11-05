//! Sizing the feed's pane in the Herdr desktop app.
//!
//! The app keeps every tab's pane tree in herdr-core's `herdr-app` document:
//! rows and columns of blocks, each with `size.weight`. A split gives both
//! sides 0.5, and the app has no command to change that, so we rewrite the two
//! weights in the document and the app follows. Writes are versioned: when
//! someone else changed the document in between, we read it again and retry.
//! The document's shape is the app's, not a published API, so anything we
//! don't recognize leaves it untouched.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

const DOC: &str = "herdr-app";
/// Versioned writes that lost to someone else's write.
const TRIES: u32 = 3;
/// How long one answer from herdr-core may take before we give up.
const ANSWER: Duration = Duration::from_secs(5);
/// How long a fresh split may take to reach the document, and how often to look.
const SETTLE: Duration = Duration::from_secs(3);
const SETTLE_STEP: Duration = Duration::from_millis(200);

struct Core {
    child: Child,
    input: ChildStdin,
    /// Lines from hc, read on their own thread so a silent core can't block us.
    lines: Receiver<String>,
    next: u64,
}

impl Core {
    fn connect() -> Result<Core> {
        let mut child = Command::new("hc")
            .arg("connect")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("starting `hc connect`")?;
        let input = child.stdin.take().context("hc stdin")?;
        let output = BufReader::new(child.stdout.take().context("hc stdout")?);
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in output.lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Core {
            child,
            input,
            lines,
            next: 1,
        })
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next;
        self.next += 1;
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.input, "{req}")?;
        self.input.flush()?;
        let deadline = Instant::now() + ANSWER;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(left) {
                Ok(l) => l,
                Err(RecvTimeoutError::Timeout) => bail!("{method}: no answer from herdr-core"),
                Err(RecvTimeoutError::Disconnected) => bail!("hc closed the connection"),
            };
            let msg: Value = serde_json::from_str(&line)?;
            if msg.get("id").and_then(Value::as_u64) != Some(id) {
                continue; // a notification
            }
            if let Some(err) = msg.get("error") {
                let kind = err
                    .pointer("/data/kind")
                    .and_then(Value::as_str)
                    .unwrap_or("error");
                return Err(anyhow!("{method}: {kind}"));
            }
            return msg.get("result").cloned().context("no result");
        }
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Give `feed` `share` of the weight it shares with `agent`, when both are blocks in one row.
/// Returns whether it found them.
pub fn reweigh(doc: &mut Value, agent: &str, feed: &str, share: f64) -> bool {
    fn terminal(n: &Value) -> Option<&str> {
        n.pointer("/content/terminal").and_then(Value::as_str)
    }
    fn weight(n: &Value) -> f64 {
        n.pointer("/size/weight")
            .and_then(Value::as_f64)
            .unwrap_or(0.5)
    }
    fn walk(n: &mut Value, agent: &str, feed: &str, share: f64) -> bool {
        let row = n.get("type").and_then(Value::as_str) == Some("row");
        let Some(kids) = n.get_mut("children").and_then(Value::as_array_mut) else {
            return false;
        };
        let a = kids.iter().position(|k| terminal(k) == Some(agent));
        let f = kids.iter().position(|k| terminal(k) == Some(feed));
        if let (true, Some(a), Some(f)) = (row, a, f) {
            let total = weight(&kids[a]) + weight(&kids[f]);
            let round = |x: f64| (x * 10_000.0).round() / 10_000.0;
            kids[a]["size"] = json!({ "weight": round(total * (1.0 - share)) });
            kids[f]["size"] = json!({ "weight": round(total * share) });
            return true;
        }
        kids.iter_mut().any(|k| walk(k, agent, feed, share))
    }
    let Some(spaces) = doc.get_mut("spaces").and_then(Value::as_array_mut) else {
        return false;
    };
    spaces.iter_mut().any(|s| {
        s.get_mut("tabs")
            .and_then(Value::as_array_mut)
            .is_some_and(|tabs| {
                tabs.iter_mut().any(|t| {
                    t.get_mut("root")
                        .is_some_and(|r| walk(r, agent, feed, share))
                })
            })
    })
}

pub fn resize(agent: &str, feed: &str, share: f64) -> Result<()> {
    let mut core = Core::connect()?;
    let settle_by = Instant::now() + SETTLE;
    let mut conflicts = 0;
    loop {
        let got = core.call("doc.get", json!({ "name": DOC }))?;
        let version = got.get("version").and_then(Value::as_u64).unwrap_or(0);
        let mut doc = got.get("value").cloned().unwrap_or(Value::Null);
        if !reweigh(&mut doc, agent, feed, share) {
            // the terminal can exist before the app has saved the split: give it a moment
            if Instant::now() >= settle_by {
                bail!("{agent} and {feed} are not side by side in one row");
            }
            std::thread::sleep(SETTLE_STEP);
            continue;
        }
        match core.call(
            "doc.set",
            json!({ "name": DOC, "value": doc, "version": version }),
        ) {
            Ok(_) => return Ok(()),
            Err(e) if e.to_string().ends_with("conflict") && conflicts + 1 < TRIES => {
                conflicts += 1
            }
            Err(e) if e.to_string().ends_with("conflict") => {
                bail!("the layout kept changing; gave up after {TRIES} tries")
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(t: &str, w: Option<f64>) -> Value {
        let mut b =
            json!({ "type": "block", "id": format!("p-{t}"), "content": { "terminal": t } });
        if let Some(w) = w {
            b["size"] = json!({ "weight": w });
        }
        b
    }
    fn doc(root: Value) -> Value {
        json!({ "spaces": [{ "id": "sp1", "tabs": [{ "id": "tb1", "root": root }] }] })
    }

    #[test]
    fn splits_a_fresh_pair_62_38() {
        let mut d = doc(
            json!({ "type": "row", "children": [block("t1", Some(0.5)), block("t2", Some(0.5))] }),
        );
        assert!(reweigh(&mut d, "t1", "t2", 0.38));
        let kids = &d["spaces"][0]["tabs"][0]["root"]["children"];
        assert_eq!(kids[0]["size"]["weight"], json!(0.62));
        assert_eq!(kids[1]["size"]["weight"], json!(0.38));
    }

    #[test]
    fn keeps_the_pair_total_when_siblings_share_the_row() {
        let mut d = doc(json!({ "type": "row", "children": [
            block("t0", Some(0.5)), block("t1", Some(0.25)), block("t2", Some(0.25))] }));
        assert!(reweigh(&mut d, "t1", "t2", 0.4));
        let kids = &d["spaces"][0]["tabs"][0]["root"]["children"];
        assert_eq!(kids[0]["size"]["weight"], json!(0.5));
        assert_eq!(kids[1]["size"]["weight"], json!(0.3));
        assert_eq!(kids[2]["size"]["weight"], json!(0.2));
    }

    #[test]
    fn finds_a_nested_pair_and_treats_missing_weights_as_half() {
        let mut d = doc(json!({ "type": "column", "children": [
            block("t9", None),
            { "type": "row", "children": [block("t1", None), block("t2", None)] }] }));
        assert!(reweigh(&mut d, "t1", "t2", 0.38));
        let row = &d["spaces"][0]["tabs"][0]["root"]["children"][1]["children"];
        assert_eq!(row[1]["size"]["weight"], json!(0.38));
        assert!(
            d["spaces"][0]["tabs"][0]["root"]["children"][0]
                .get("size")
                .is_none()
        );
    }

    #[test]
    fn leaves_a_column_pair_and_unknown_shapes_alone() {
        let mut col =
            doc(json!({ "type": "column", "children": [block("t1", None), block("t2", None)] }));
        let before = col.clone();
        assert!(!reweigh(&mut col, "t1", "t2", 0.38));
        assert_eq!(col, before);
        assert!(!reweigh(&mut json!(null), "t1", "t2", 0.38));
        assert!(!reweigh(&mut json!({ "spaces": "x" }), "t1", "t2", 0.38));
    }
}

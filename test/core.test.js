import { test } from "node:test";
import assert from "node:assert/strict";
import { decide, findFeed, canOpenBeside } from "../mods/herdr-feed/lib/core.js";

const agent = (address, status, focused = false, tab = "tb1") => ({ address, status, focused, tab });
const focus = (terminal, tab = "tb1", pane = "p1") => ({ terminal, tab, pane });
const types = (out) => out.actions.map((a) => a.type);
const base = { enabled: true, feed: null, feedTab: null, anchor: null, dismissed: null, shown: null };

test("finds the feed by its name or its command line", () => {
  assert.equal(findFeed([{ address: "t1", argv: ["zsh"] }, { address: "t2", name: "herdr-feed" }]), "t2");
  assert.equal(findFeed([{ address: "t3", argv: ["/Users/x/.local/bin/herdr-feed", "pane", "--key", "k"] }]), "t3");
  assert.equal(findFeed([{ address: "t4", argv: ["herdr-feed", "fetch"] }]), null);
  assert.equal(findFeed([{ address: "t5", argv: ["vim", "herdr-feed"] }]), null);
  assert.equal(findFeed(undefined), null);
});

test("opens beside the focused agent once it works", () => {
  const out = decide({ ...base, agents: [agent("t1", "working", true)], focused: focus("t1") });
  assert.deepEqual(out.actions, [{ type: "open", agent: "t1" }]);
  assert.equal(out.anchor, "t1");
});

test("does not open for an idle agent, an unfocused one, or when off", () => {
  assert.deepEqual(types(decide({ ...base, agents: [agent("t1", "idle", true)], focused: focus("t1") })), []);
  assert.deepEqual(types(decide({ ...base, agents: [agent("t1", "working", false)], focused: focus("t9") })), []);
  assert.deepEqual(types(decide({ ...base, enabled: false, agents: [agent("t1", "working", true)], focused: focus("t1") })), []);
});

test("does not open while a layout block, not a pane, has focus", () => {
  const out = decide({ ...base, agents: [agent("t1", "working", true)], focused: { terminal: "t1", tab: "tb1", pane: null } });
  assert.deepEqual(types(out), []);
});

test("closes when its agent stops working", () => {
  for (const status of ["idle", "done", "blocked"]) {
    const out = decide({ ...base, feed: "t2", feedTab: "tb1", anchor: "t1", agents: [agent("t1", status, true)], focused: focus("t1") });
    assert.deepEqual(types(out), ["close"], status);
    assert.equal(out.anchor, null);
  }
});

test("closes when its agent is gone or the switch is turned off", () => {
  assert.deepEqual(types(decide({ ...base, feed: "t2", anchor: "t1", agents: [], focused: focus("t9") })), ["close"]);
  assert.deepEqual(types(decide({ ...base, enabled: false, feed: "t2", anchor: "t1", agents: [agent("t1", "working", true)], focused: focus("t1") })), ["close"]);
});

test("closes when another agent becomes the active one", () => {
  const out = decide({
    ...base, feed: "t2", feedTab: "tb1", anchor: "t1", shown: "running",
    agents: [agent("t1", "working"), agent("t3", "working", true)], focused: focus("t3"),
  });
  assert.deepEqual(types(out), ["close"]);
});

test("stays open, running, while you work in a plain pane of the same tab", () => {
  const out = decide({ ...base, feed: "t2", feedTab: "tb1", anchor: "t1", shown: "running", agents: [agent("t1", "working")], focused: focus("t5") });
  assert.deepEqual(types(out), []);
  assert.equal(out.anchor, "t1");
});

test("holds its cards while you're in it, pauses in another tab, and runs again after", () => {
  const s = { ...base, feed: "t2", feedTab: "tb1", anchor: "t1", agents: [agent("t1", "working")] };
  assert.deepEqual(decide({ ...s, shown: "running", focused: focus("t2") }).actions, [{ type: "state", state: "browsing" }]);
  assert.deepEqual(decide({ ...s, shown: "running", focused: focus("t7", "tb2") }).actions, [{ type: "state", state: "paused" }]);
  assert.deepEqual(decide({ ...s, shown: "browsing", focused: focus("t7", "tb2") }).actions, [{ type: "state", state: "paused" }]);
  assert.deepEqual(decide({ ...s, shown: "browsing", focused: focus("t2") }).actions, []);
  const back = { ...s, agents: [agent("t1", "working", true)], shown: "browsing", focused: focus("t1") };
  assert.deepEqual(decide(back).actions, [{ type: "state", state: "running" }]);
});

test("a feed closed by hand stays shut for the rest of that turn", () => {
  const working = { ...base, agents: [agent("t1", "working", true)], focused: focus("t1") };
  const gone = decide({ ...working, anchor: "t1" });
  assert.deepEqual(types(gone), []);
  assert.equal(gone.dismissed, "t1");
  assert.deepEqual(types(decide({ ...working, dismissed: "t1" })), []);
  // the turn ends: the dismissal goes, and the next turn opens again
  const idle = decide({ ...base, agents: [agent("t1", "idle", true)], focused: focus("t1"), dismissed: "t1" });
  assert.equal(idle.dismissed, null);
  assert.deepEqual(types(decide({ ...working, dismissed: idle.dismissed })), ["open"]);
});

test("a dismissal for one agent does not block another", () => {
  const out = decide({ ...base, agents: [agent("t1", "working"), agent("t3", "working", true)], focused: focus("t3"), dismissed: "t1" });
  assert.deepEqual(out.actions, [{ type: "open", agent: "t3" }]);
  assert.equal(out.dismissed, "t1");
});

test("a feed opens beside an agent only while it works and its pane has focus", () => {
  const list = [agent("t1", "working", true), agent("t3", "idle")];
  assert.equal(canOpenBeside("t1", list, focus("t1")), true);
  assert.equal(canOpenBeside("t1", list, focus("t3")), false, "focus moved to another agent");
  assert.equal(canOpenBeside("t1", list, focus("t5")), false, "focus moved to a plain pane");
  assert.equal(canOpenBeside("t1", list, { terminal: "t1", tab: "tb1", pane: null }), false, "a layout block");
  assert.equal(canOpenBeside("t3", list, focus("t3")), false, "not working");
  assert.equal(canOpenBeside("t9", list, focus("t9")), false, "gone");
  assert.equal(canOpenBeside("t1", list, null), false);
});

test("the pane command keeps matching the feed with --driven", () => {
  assert.equal(findFeed([{ address: "t6", argv: ["/x/herdr-feed", "pane", "--key", "k", "--driven"] }]), "t6");
});

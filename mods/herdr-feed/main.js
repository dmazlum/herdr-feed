// herdr-feed: short tech cards beside the active coding agent while it works.
//
// `mod.open:herdr-feed` puts a small window on screen: the switch. While that
// window is open it watches the agents. When the focused agent starts working
// it opens `herdr-feed pane` in a split to its right (keeping your focus on the
// agent), pauses it while you look at it, and closes it when the agent stops
// working or you move to another agent. The cards, images and video are drawn
// by the herdr-feed binary with the kitty graphics protocol.

import { View, div } from "gpui-kit";
import { h_flex } from "gpui-base";
import {
  exec, run, theme, agents, focused, terminals, panes, home, props,
  openOverlay, closeOverlay, shared, setShared, nextCommand, notify,
} from "herdr";
import { decide, findFeed, canOpenBeside, FEED_NAME, STATE_KEY } from "./lib/core.js";

const BIN = () => `${home()}/.local/bin/herdr-feed`;
const ENABLED = "herdr-feed.enabled";
const WINDOW_KEY = "herdr-feed";
const TICK_MS = 1000;
// How long a new feed has to show up in terminals(), and to exit after being told to close.
const APPEAR_MS = 4000;
const EXIT_MS = 1500;

const enabled = () => shared(ENABLED) !== false;

export default class Feed extends View {
  init(_props, cx) {
    const p = props() ?? {};
    // Opened by mod.open: hand over to the kept window and close this launcher.
    if (!p.controller) {
      this.launcher = true;
      openOverlay({
        mod: "herdr-feed", props: { controller: true },
        kind: "window", key: WINDOW_KEY, keep: true, width: 150, height: 34,
      });
      if (p.overlay) closeOverlay(p.overlay);
      return;
    }
    this.feed = null;
    this.anchor = null;
    this.dismissed = null;
    this.paused = null;
    this.busy = false;
    this.note = null; // "no cards", "not installed"
    // A feed left over from an earlier copy has no anchor we know: start clean.
    const stale = findFeed(terminals());
    if (stale) run("terminal.kill", { terminal: stale });
    cx.timer.every(TICK_MS, (cx) => this.tick(cx));
    cx.spawn(async (cx) => {
      for (;;) {
        const c = await nextCommand();
        if (c === "toggle") this.toggle();
        cx.notify();
      }
    });
  }

  toggle() {
    setShared(ENABLED, !enabled());
  }

  tick(cx) {
    if (this.busy) return;
    const feed = findFeed(terminals());
    const feedTab = feed ? panes().find((p) => p.terminal === feed)?.tab ?? null : null;
    if (!feed) this.paused = null;
    const out = decide({
      enabled: enabled(), agents: agents(), focused: focused(),
      feed, feedTab, anchor: this.anchor, dismissed: this.dismissed, paused: this.paused,
    });
    const before = `${this.feed}|${this.note}`;
    this.feed = feed;
    this.anchor = out.anchor;
    this.dismissed = out.dismissed;
    if (out.actions.length) {
      this.busy = true;
      cx.spawn(async (cx) => {
        try {
          for (const a of out.actions) await this.act(cx, a, feed);
        } catch (e) {
          console.log(`herdr-feed: ${e}`);
        } finally {
          this.busy = false;
          cx.notify();
        }
      });
    } else if (before !== `${this.feed}|${this.note}`) {
      cx.notify();
    }
  }

  async act(cx, a, feed) {
    if (a.type === "pause") {
      await exec(BIN(), ["state", STATE_KEY, a.paused ? "paused" : "running"]);
      this.paused = a.paused;
    } else if (a.type === "close") {
      await exec(BIN(), ["state", STATE_KEY, "closed"]).catch(() => null);
      if (await this.waitFor(cx, () => !findFeed(terminals()), EXIT_MS) === false) {
        run("terminal.kill", { terminal: feed });
      }
      this.feed = null;
      this.paused = null;
    } else if (a.type === "open") {
      await this.open(cx, a.agent);
    }
  }

  async open(cx, agent) {
    let ready;
    try {
      ready = await exec(BIN(), ["ready"]);
    } catch {
      this.fail(agent, "not installed");
      return;
    }
    if (ready.code !== 0) {
      this.fail(agent, "no cards");
      return;
    }
    // ready may have started a background fetch; the cards it found are enough for now.
    await exec(BIN(), ["state", STATE_KEY, "running"]);
    this.note = null;
    // The awaits above took time: the split goes beside whatever has focus now, so only go on if
    // that is still this agent, still working. Otherwise the next tick decides afresh.
    if (!canOpenBeside(agent, agents(), focused())) {
      this.anchor = null;
      return;
    }
    run("terminal.open", { argv: [BIN(), "pane", "--key", STATE_KEY, "--driven"], place: "right", name: FEED_NAME });
    run("pane.focus", { terminal: agent });
    const appeared = await this.waitFor(cx, () => findFeed(terminals()), APPEAR_MS);
    if (!appeared) {
      this.fail(agent, "could not open");
      return;
    }
    this.feed = appeared;
    this.paused = false;
    // A split is 50/50; the binary narrows the feed to `pane_ratio` (38%) in the app's layout document.
    await exec(BIN(), ["resize", agent, appeared]).catch(() => null);
  }

  /** No feed for this turn of `agent`, and say why on the switch. */
  fail(agent, why) {
    this.anchor = null;
    this.dismissed = agent;
    if (why === "not installed" && this.note !== why) {
      notify({ title: "herdr-feed", body: `Not found at ${BIN()}. See the README to install it.`, level: "warning" });
    }
    this.note = why;
  }

  /** Polls `check` until it returns something truthy or `ms` pass; returns that value or false. */
  async waitFor(cx, check, ms) {
    const end = Date.now() + ms;
    for (;;) {
      const v = check();
      if (v) return v;
      if (Date.now() > end) return false;
      await new Promise((done) => cx.timer.after(150, () => done(null)));
    }
  }

  async close(_cx) {
    if (this.launcher) return;
    const feed = findFeed(terminals());
    if (feed) run("terminal.kill", { terminal: feed });
  }

  render(_cx) {
    const t = theme();
    if (this.launcher) return div();
    const on = enabled();
    const [word, color] = !on ? ["off", t.dim]
      : this.feed ? [this.paused ? "paused" : "showing", this.paused ? t.idle : t.working]
      : this.note ? [this.note, t.blocked]
      : ["on", t.accent];
    return h_flex().id("root").size_full().px(10).gap(8).items_center().text_size(12)
      .child(div().text_color(t.dim).child("⠿"))
      .child(h_flex().id("switch").flex_1().gap(7).items_center().cursor_pointer()
        .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
        .on_click((_e, cx) => { this.toggle(); cx.notify(); })
        .child(div().w(7).h(7).rounded_full().bg(color))
        .child(div().font_semibold().text_color(on ? t.foreground : t.muted).child("feed"))
        .child(div().flex_1().truncate().text_color(t.muted).child(word)));
  }
}

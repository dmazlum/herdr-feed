// The decisions behind herdr-feed, kept free of Herdr calls so they can be tested.
//
// One feed at a time, beside the active agent: the agent whose pane has focus.
// It opens when that agent starts working, pauses while you look at it (or at
// another tab), and closes when its agent stops working or you move to another
// agent. Closing it by hand (q) keeps it shut until that agent's next turn.

export const FEED_NAME = "herdr-feed";
export const STATE_KEY = "herdr-app";

/** The feed's terminal among `terminals()`: named by us, or running `herdr-feed pane`. */
export function findFeed(terminals) {
  for (const t of terminals ?? []) {
    if (!t?.address) continue;
    if (t.name === FEED_NAME) return t.address;
    const argv = Array.isArray(t.argv) ? t.argv : [];
    const i = argv.findIndex((a) => typeof a === "string" && /(^|\/)herdr-feed$/.test(a));
    if (i >= 0 && argv[i + 1] === "pane") return t.address;
  }
  return null;
}

/**
 * A split goes beside the focused pane, so a feed can open beside `agent` only while that agent
 * works and its own pane (not a layout block showing it) has focus.
 */
export function canOpenBeside(agent, agents, focused) {
  const a = (agents ?? []).find((x) => x.address === agent);
  return Boolean(a?.status === "working" && focused?.pane && focused.terminal === agent);
}

/**
 * What to do now.
 *
 * input:
 *   enabled    the person's on/off
 *   agents     agents() — { address, status, focused, tab }
 *   focused    focused() — { terminal, pane, tab } or null
 *   feed       the feed's terminal address, or null
 *   feedTab    the tab the feed's pane is in, or null
 *   anchor     the agent the feed was opened beside, or null
 *   dismissed  an agent whose current turn should not get a feed (closed by hand, no cards)
 *   paused     what the feed was last told: true paused, false running
 *
 * Returns { actions, anchor, dismissed }. Actions, in order:
 *   { type: "close" }                         tell the feed to close, then kill it
 *   { type: "open", agent }                   open a feed beside that agent
 *   { type: "pause", paused }                 tell the open feed to pause or resume
 */
export function decide(input) {
  const { enabled, agents = [], focused = null, feed = null, feedTab = null } = input;
  let { anchor = null, dismissed = null } = input;
  const actions = [];
  const byAddress = (a) => agents.find((x) => x.address === a) ?? null;
  const working = (a) => a?.status === "working";

  // A dismissal lasts for one turn: it ends once that agent stops working.
  if (dismissed && !working(byAddress(dismissed))) dismissed = null;

  const active = agents.find((a) => a.focused) ?? null;

  if (feed) {
    const anchored = byAddress(anchor);
    const switched = active && active.address !== anchor;
    if (!enabled || !working(anchored) || switched) {
      actions.push({ type: "close" });
      return { actions, anchor: null, dismissed };
    }
    const looking = focused?.terminal === feed;
    const elsewhere = Boolean(feedTab && focused?.tab && focused.tab !== feedTab);
    const paused = looking || elsewhere;
    if (paused !== input.paused) actions.push({ type: "pause", paused });
    return { actions, anchor, dismissed };
  }

  // The feed ended on its own (q, no more cards) while its agent still works: not again this turn.
  if (anchor) {
    if (working(byAddress(anchor))) dismissed = anchor;
    anchor = null;
  }

  if (enabled && active && canOpenBeside(active.address, agents, focused) && dismissed !== active.address) {
    actions.push({ type: "open", agent: active.address });
    anchor = active.address;
  }
  return { actions, anchor, dismissed };
}

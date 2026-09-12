/// <reference path="../src/rules/rules.d.ts" />

const COMMIT_COMMAND_RE = /^\s*(?:(?:gdf-)?commit\s+)?[\/$](?:gdf-)?commit\b/;

// use the 'commit' skill with arguments
// use the 'gdf-commit' skill with arguments
const COMMIT_SKILL_REQUEST_RE = /^\s*use the ['"]?(?:gdf-)?commit['"]?\s+skill\b/;

const COMMIT_SKILL_REPLY_RE = /^\s*(?:y|yes|no|n)\s*$/i;

/**
 * Trashes short commit helper sessions to avoid cluttering the chat history.
 * This rule specifically targets sessions using models with "-spark" in their name
 */
rule("commit tool short session", { applyAtStartup: true }, (ctx) => {
  const { turns, session, session: { model, agent } } = ctx;


  const isSpark = model.includes("-codex-spark");
  const isClaude = agent.includes("claude");
  const isCodex = agent.includes("codex");
  const isAgy = agent.includes("antigravity");
  // let isLowOrMediumEffortGPT = model.includes("gpt-") && (session.reasoningEffort == "low" || session.reasoningEffort == "medium");


  if (turns.user.length == 0) {
    if (isAgy) return trash("agy session has 0 user turns");
    if (isSpark || isClaude || isCodex) return trash("session has 0 user turns");
    return nothing();
  }

  const ut0t = turns.user[0].text();
  if ((isSpark || isClaude || isCodex)
    && COMMIT_COMMAND_RE.test(ut0t)) {
    if (turns.user.length > 3) return nothing("session has more than 3 user turns");
    return trash(`short (${turns.user.length} user turns) commit`);
  }

  if (isAgy && COMMIT_SKILL_REQUEST_RE.test(ut0t)) {
    if (turns.user.length > 12) return nothing("agy session has more than 12 user turns");
    return trash(`commit skill session (u=${turns.user.length} a=${turns.agent.length})`);
  }

  return nothing();
  //     let intro = ut0t.substring(0, 64);
  //     let outro = ut0t.substring(ut0t.length - 64, ut0t.length);
  //     let utlt = turns.user[turns.user.length - 1].text();
  //     let utlti = utlt.substring(0, 64);
  //     let utlto = utlt.substring(utlt.length - 64, utlt.length);
  //     //         return trash(`
  //     // =============${turns.user.length}===================
  //     // ut0t[0:64]=[${intro}] //// ut0t[-64:]=[${outro}]
  //     // ----------------------------------------------------
  //     // utlt[0:64]=[${utlti}] //// utlt[-64:]=[${utlto}]
  //     // @@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@
  //     // `);
  // }
});

rule("trash commit-only sessions", { applyAtStartup: false }, ({ turns }) => {
  if (turns.user.length === 0) return nothing();

  const isCommitOnly = turns.user.every((turn) => {
    const text = turn.text();
    return COMMIT_COMMAND_RE.test(text)
      || COMMIT_SKILL_REQUEST_RE.test(text)
      || COMMIT_SKILL_REPLY_RE.test(text);
  });

  return isCommitOnly
    ? trash(`commit-only session (${turns.user.length} user turns)`)
    : nothing();
});

rule("trash superseded sessions", { applyAtStartup: false }, ({ session }) => {
  return session.supersededBy
    ? trash(`superseded by ${session.supersededBy}`)
    : nothing();
});

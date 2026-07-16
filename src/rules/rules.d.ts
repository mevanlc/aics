/** AICS rules API declarations. Install with `aics --write-rules-dts`. */

/** CLI that created the session. */
type AicsAgent = "claude" | "codex";

/** Relationship between this session and its source session, if any. */
type AicsDerivationType = "original" | "trimmed" | "continued" | "sub_agent";

/** Operation performed on a file by a patch. */
type AicsPatchOp = "add" | "update" | "delete";

/** Values and lazy transcript accessors passed to each rule callback. */
interface AicsRuleContext {
  /** Metadata for the session being evaluated. */
  session: AicsRuleSession;

  /** Parsed transcript turns grouped by kind. */
  turns: AicsRuleTurns;
}

/** Session metadata available to rules. */
interface AicsRuleSession {
  /** Session identifier recorded by Claude Code or Codex CLI. */
  id: string;

  /** CLI that created the session. */
  agent: AicsAgent;

  /** Project identifier derived from the session. */
  project: string;

  /** Session working directory, or an empty string when unavailable. */
  cwd: string;

  /** Git branch, or an empty string when unavailable. */
  branch: string;

  /** Path of the normal or trashed JSONL session file being evaluated. */
  path: string;

  /** Last-modified time as Unix seconds. */
  modifiedTs: number;

  /** Number of source JSONL lines. */
  lines: number;

  /** Whether the session is original, trimmed, continued, or a sub-agent. */
  derivationType: AicsDerivationType;

  /** Whether the source marks this session as a sidechain/sub-agent session. */
  isSidechain: boolean;

  /** User-assigned title, or an empty string when unavailable. */
  customTitle: string;

  /** Model name, or an empty string when unavailable. */
  model: string;

  /** Model provider, or an empty string when unavailable. */
  modelProvider: string;

  /** Reasoning effort, or an empty string when unavailable. */
  reasoningEffort: string;

  /** Approval policy, or an empty string when unavailable. */
  approvalPolicy: string;

  /** Sandbox mode, or an empty string when unavailable. */
  sandboxMode: string;

  /** Whether this file is currently in the AICS trash store. */
  trashed: boolean;
}

/** Parsed transcript turns grouped for convenient rule matching. */
interface AicsRuleTurns {
  /** User messages, excluding automatically injected contextual messages. */
  user: AicsTextTurn[];

  /** Automatically injected user-role context such as AGENTS.md content. */
  contextualUser: AicsTextTurn[];

  /** Assistant messages and reasoning blocks. */
  agent: AicsTextTurn[];

  /** System and summary messages. */
  system: AicsTextTurn[];

  /** Structured tool calls. */
  toolCalls: AicsToolCallTurn[];

  /** Structured tool results. */
  toolResults: AicsToolResultTurn[];

  /** Structured command executions. */
  exec: AicsExecTurn[];

  /** Structured file patches. */
  patches: AicsPatchTurn[];
}

/** A user, contextual-user, assistant, reasoning, system, or summary turn. */
interface AicsTextTurn {
  /** Original index in the parsed session-cell sequence. */
  index: number;

  /** RFC 3339 timestamp, or `null` when unavailable. */
  timestamp: string | null;

  /**
   * Lazily returns this turn's text.
   * @param limit Maximum source-text UTF-8 bytes before truncation. Omit for the runtime default.
   */
  text(limit?: number): string;
}

/** A structured tool invocation. */
interface AicsToolCallTurn {
  /** Original index in the parsed session-cell sequence. */
  index: number;

  /** Normalized tool name. */
  tool: string;

  /** Compact description of the invocation. */
  summary: string;

  /** RFC 3339 timestamp, or `null` when unavailable. */
  timestamp: string | null;
}

/** Output from a structured tool invocation. */
interface AicsToolResultTurn {
  /** Original index in the parsed session-cell sequence. */
  index: number;

  /** Normalized tool name, or `null` when it could not be paired. */
  tool: string | null;

  /** Whether the tool result represents an error. */
  isError: boolean;

  /** RFC 3339 timestamp, or `null` when unavailable. */
  timestamp: string | null;

  /**
   * Lazily returns the tool output.
   * @param limit Maximum source-text UTF-8 bytes before truncation. Omit for the runtime default.
   */
  text(limit?: number): string;
}

/** A structured command execution. */
interface AicsExecTurn {
  /** Original index in the parsed session-cell sequence. */
  index: number;

  /** Command strings captured for the execution. */
  command: string[];

  /** Execution working directory, or `null` when unavailable. */
  cwd: string | null;

  /** Process exit code, or `null` when unavailable. */
  exitCode: number | null;

  /** RFC 3339 timestamp, or `null` when unavailable. */
  timestamp: string | null;

  /**
   * Lazily returns captured standard output.
   * @param limit Maximum source-text UTF-8 bytes before truncation. Omit for the runtime default.
   */
  stdout(limit?: number): string;

  /**
   * Lazily returns captured standard error.
   * @param limit Maximum source-text UTF-8 bytes before truncation. Omit for the runtime default.
   */
  stderr(limit?: number): string;
}

/** A structured patch operation. */
interface AicsPatchTurn {
  /** Original index in the parsed session-cell sequence. */
  index: number;

  /** Files affected by the patch. */
  files: AicsPatchFile[];

  /** Whether the patch was applied successfully. */
  success: boolean;

  /** RFC 3339 timestamp, or `null` when unavailable. */
  timestamp: string | null;
}

/** One file affected by a structured patch. */
interface AicsPatchFile {
  /** File path recorded by the patch. */
  path: string;

  /** File operation performed by the patch. */
  op: AicsPatchOp;

  /** Number of added lines reported by the parser. */
  additions: number;

  /** Number of deleted lines reported by the parser. */
  deletions: number;

  /**
   * Lazily returns post-change content when available.
   * Deleted files and source formats without captured content may return an
   * empty string.
   *
   * @param limit Maximum source-text UTF-8 bytes before truncation. Omit for the runtime default.
   */
  content(limit?: number): string;
}

/** An action that a rule callback may return, alone or in an array. */
type AicsRuleAction = AicsNothingAction | AicsTrashAction | AicsUntrashAction;

/** Optional behavior for a registered rule. */
interface AicsRuleConfig {
  /**
   * Apply this rule during ordinary AICS startup.
   * Startup rules evaluate globally over normal, non-trashed sessions.
   * Explicit preview/apply modes evaluate every registered rule.
   */
  applyAtStartup?: boolean;

  /** Additional configuration keys are accepted for forward compatibility. */
  [key: string]: unknown;
}

/** Explicitly proposes no action. `null` and `undefined` have the same effect. */
interface AicsNothingAction {
  /** No-op action discriminator. */
  action: "nothing";
}

/** Proposes moving the session into the AICS trash store. */
interface AicsTrashAction {
  /** Trash action discriminator. */
  action: "trash";

  /** Optional human-readable explanation included in previews and reports. */
  reason: string | null;
}

/** Proposes restoring a trashed session to its recorded original path. */
interface AicsUntrashAction {
  /** Untrash action discriminator. */
  action: "untrash";

  /** Optional human-readable explanation included in previews and reports. */
  reason: string | null;
}

/**
 * Registers a rule evaluated by explicit preview/apply modes.
 *
 * A callback may return one action, an array of actions, `null`, or `undefined`.
 * Rule names must be non-empty and unique within the rules file.
 *
 * @param name Human-readable unique rule name.
 * @param callback Function that evaluates one session.
 */
declare function rule(
  name: string,
  callback: (context: AicsRuleContext) => AicsRuleAction | AicsRuleAction[] | null | undefined,
): void;

/**
 * Registers a configured rule.
 *
 * @param name Human-readable unique rule name.
 * @param config Rule behavior such as automatic startup application.
 * @param callback Function that evaluates one session.
 */
declare function rule(
  name: string,
  config: AicsRuleConfig,
  callback: (context: AicsRuleContext) => AicsRuleAction | AicsRuleAction[] | null | undefined,
): void;

/** Returns an explicit no-op action. */
declare function nothing(): AicsNothingAction;

/**
 * Returns an action that moves the session into the AICS trash store.
 * Applying it to an already-trashed session is skipped.
 *
 * @param reason Optional human-readable explanation.
 */
declare function trash(reason?: string): AicsTrashAction;

/**
 * Returns an action that restores a trashed session to its original path.
 * Applying it to a normal session is skipped as already untrashed.
 *
 * @param reason Optional human-readable explanation.
 */
declare function untrash(reason?: string): AicsUntrashAction;

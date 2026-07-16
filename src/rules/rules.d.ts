/* AICS rules API declarations. Install with `aics --write-rules-dts`. */

type AicsAgent = "claude" | "codex";
type AicsDerivationType = "original" | "trimmed" | "continued" | "sub_agent";
type AicsPatchOp = "add" | "update" | "delete";

interface AicsRuleContext {
  session: AicsRuleSession;
  turns: AicsRuleTurns;
  re(pattern: string, flags?: string): RegExp;
  text(kind: string, index: number, field?: string, limit?: number): string;
  turnText(kind: string, index: number, field?: string, limit?: number): string;
}

interface AicsRuleSession {
  id: string;
  agent: AicsAgent;
  project: string;
  cwd: string;
  branch: string;
  path: string;
  modifiedTs: number;
  lines: number;
  derivationType: AicsDerivationType;
  isSidechain: boolean;
  customTitle: string;
  model: string;
  modelProvider: string;
  reasoningEffort: string;
  approvalPolicy: string;
  sandboxMode: string;
  trashed: boolean;
  firstUserText(limit?: number): string;
  firstText(limit?: number): string;
  lastText(limit?: number): string;
}

interface AicsRuleTurns {
  user: AicsTextTurn[];
  contextualUser: AicsTextTurn[];
  agent: AicsTextTurn[];
  system: AicsTextTurn[];
  toolCalls: AicsToolCallTurn[];
  toolResults: AicsToolResultTurn[];
  exec: AicsExecTurn[];
  patches: AicsPatchTurn[];
}

interface AicsTextTurn {
  index: number;
  timestamp: string | null;
  text(limit?: number): string;
}

interface AicsToolCallTurn {
  index: number;
  tool: string;
  summary: string;
  timestamp: string | null;
}

interface AicsToolResultTurn {
  index: number;
  tool: string | null;
  isError: boolean;
  timestamp: string | null;
  text(limit?: number): string;
}

interface AicsExecTurn {
  index: number;
  command: string[];
  cwd: string | null;
  exitCode: number | null;
  timestamp: string | null;
  stdout(limit?: number): string;
  stderr(limit?: number): string;
}

interface AicsPatchTurn {
  index: number;
  files: AicsPatchFile[];
  success: boolean;
  timestamp: string | null;
}

interface AicsPatchFile {
  path: string;
  op: AicsPatchOp;
  additions: number;
  deletions: number;
  content(limit?: number): string;
}

type AicsRuleAction = AicsNothingAction | AicsTrashAction | AicsUntrashAction;
interface AicsRuleConfig {
  applyAtStartup?: boolean;
  [key: string]: unknown;
}

interface AicsNothingAction {
  action: "nothing";
}

interface AicsTrashAction {
  action: "trash";
  reason: string | null;
}

interface AicsUntrashAction {
  action: "untrash";
  reason: string | null;
}

declare function rule(
  name: string,
  callback: (context: AicsRuleContext) => AicsRuleAction | AicsRuleAction[] | null | undefined,
): void;
declare function rule(
  name: string,
  config: AicsRuleConfig,
  callback: (context: AicsRuleContext) => AicsRuleAction | AicsRuleAction[] | null | undefined,
): void;
declare function nothing(): AicsNothingAction;
declare function trash(reason?: string): AicsTrashAction;
declare function untrash(reason?: string): AicsUntrashAction;

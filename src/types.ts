export interface Project { id:string; name:string; path:string; language:string; buildSystem:string; testCommand:string; benchmarkCommand:string; lastUsedAt:string }
export interface RepoInspection { path:string; branch:string; headSha:string; dirty:boolean; language:string; buildSystem:string; suggestedTestCommand:string }
export interface StartRunRequest { projectId:string; task:string; testCommand:string; benchmarkCommand?:string; maxRepairs:number; mockAgents:boolean; agentMode:"duet"|"codex"|"claude";executionLocation:"local"|"cloud";codexModel:string;claudeModel:string;codexReasoning:string;claudeReasoning:string }
export interface RunSummary { id:string; projectId:string; projectName:string; task:string; status:string; currentStage:string; createdAt:string; completedAt?:string; worktreePath?:string; additions:number; deletions:number; appliedAt?:string; discardedAt?:string; error?:string }
export interface StageRecord { id:number; runId:string; kind:string; agent:string; status:string; summary:string; rawOutput:string; normalizedOutput:string; startedAt:string; completedAt?:string; durationMs?:number }
export interface ChangedFile { path:string; status:string; additions:number; deletions:number }
export interface VerificationResult { name:string; command:string; success:boolean; exitCode?:number; stdout:string; stderr:string; durationMs:number; required:boolean }
export interface RunDetail extends RunSummary { stages:StageRecord[]; architecture?:string; review?:string; verification:VerificationResult[]; changedFiles:ChangedFile[] }
export interface ToolStatus { installed:boolean; authenticated?:boolean; path?:string; version?:string; detail:string }
export interface DoctorReport { appDataWritable:boolean; databaseHealthy:boolean; git:ToolStatus; claude:ToolStatus; codex:ToolStatus; os:string }
export interface CodexModelInfo { id:string; model:string; displayName:string; hidden:boolean; defaultReasoningEffort?:string; supportedReasoningEfforts:{reasoningEffort:string;description?:string}[]; inputModalities:string[]; supportsPersonality:boolean; isDefault:boolean; upgrade?:string }
export interface CodexThreadInfo { id:string; sessionId?:string; preview?:string; ephemeral:boolean; modelProvider?:string; createdAt?:number }
export interface CodexTurnInfo { id:string; status:string; items:unknown[]; error?:unknown }
export interface CodexRuntimeEnvelope { sequence:number; event:
  | {kind:"notification";method:string;params:Record<string,unknown>}
  | {kind:"serverRequest";token:string;method:string;params:Record<string,unknown>}
  | {kind:"serverRequestResolved";token:string;resolution:string}
  | {kind:"serverRequestRejected";method:string;reason:string}
  | {kind:"notificationStreamLagged";skipped:number}
  | {kind:"fatalProtocolError";message:string}
  | {kind:"shuttingDown"}
  | {kind:"closed"} }

export type RunEvent =
  | {type:"runStarted";runId:string;task:string}
  | {type:"stageStarted";runId:string;stage:string;agent:string}
  | {type:"agentOutput";runId:string;stage:string;stream:string;line:string}
  | {type:"stageCompleted";runId:string;stage:string;success:boolean;summary:string}
  | {type:"fileChanged";runId:string;path:string}
  | {type:"verificationCompleted";runId:string;result:VerificationResult}
  | {type:"reviewCompleted";runId:string;verdict:string;issues:number}
  | {type:"runCompleted";runId:string;verified:boolean}
  | {type:"runFailed";runId:string;reason:string}
  | {type:"runCancelled";runId:string};

export type DetailTab = "summary"|"activity"|"files"|"diff"|"tests"|"review"|"logs";
export interface LiveLog { text:string; receivedAt:string }
export interface ConsoleOutputEvent { operationId:string; stream:string; chunk:string }

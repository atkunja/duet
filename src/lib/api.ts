import { invoke } from "@tauri-apps/api/core";
import type { AppPreferences, CodexModelInfo, CodexThreadInfo, CodexTurnInfo, DoctorReport, Project, RepoInspection, RunDetail, RunSummary, StartRunRequest, VerificationResult } from "../types";
import { demoDoctor, demoProject, demoRun, demoRuns, demoVerification } from "./demo";

export const isTauriRuntime = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
export const isDevelopmentPreview = !isTauriRuntime && import.meta.env.DEV;

function nativeOrDemo<T>(nativeCall: () => Promise<T>, demoCall: () => T | Promise<T>): Promise<T> {
  if (isTauriRuntime) return nativeCall();
  if (isDevelopmentPreview) return Promise.resolve().then(demoCall);
  return Promise.reject(new Error("Duet native services are unavailable"));
}

export const api = {
  inspectRepository: (path:string) => invoke<RepoInspection>("inspect_repository",{path}),
  addProject: (path:string) => invoke<Project>("add_project",{path}),
  removeProject: (projectId:string) => invoke<void>("remove_project",{projectId}),
  listProjects: () => nativeOrDemo(() => invoke<Project[]>("list_projects"), () => [demoProject]),
  startRun: (request:StartRunRequest) => invoke<string>("start_run",{request}),
  cancelRun: (runId:string) => invoke<void>("cancel_run",{runId}),
  listRuns: () => nativeOrDemo(() => invoke<RunSummary[]>("list_runs"), () => demoRuns),
  getRun: (runId:string) => nativeOrDemo(() => invoke<RunDetail>("get_run",{runId}), () => demoRun(runId)),
  getDiff: (runId:string) => nativeOrDemo(() => invoke<string>("get_diff",{runId}), () => ""),
  runProjectCommand: (projectId:string,command:string,operationId:string) => nativeOrDemo(
    () => invoke<VerificationResult>("run_project_command",{projectId,command,operationId}),
    () => demoVerification(command),
  ),
  cancelProjectCommand: (operationId:string) => invoke<void>("cancel_project_command",{operationId}),
  openLocalPreview: (url:string) => invoke<void>("open_local_preview",{url}),
  openProjectInEditor: (projectId:string) => nativeOrDemo(() => invoke<string>("open_project_in_editor",{projectId}), () => "Development preview"),
  openRunInEditor: (runId:string) => nativeOrDemo(() => invoke<string>("open_run_in_editor",{runId}), () => "Development preview"),
  applyChanges: (runId:string) => invoke<void>("apply_changes",{runId}),
  discardRun: (runId:string) => invoke<void>("discard_run",{runId}),
  doctor: () => nativeOrDemo(() => invoke<DoctorReport>("doctor"), () => demoDoctor),
  getPreferences: () => nativeOrDemo<AppPreferences>(() => invoke<AppPreferences>("get_preferences"), () => ({editor:"auto",maxRepairs:3})),
  savePreferences: (preferences:AppPreferences) => nativeOrDemo(() => invoke<void>("save_preferences",{preferences}), () => undefined),
  listCodexModels: () => nativeOrDemo(
    () => invoke<CodexModelInfo[]>("list_codex_models"),
    () => [
      { id:"gpt-5.6-sol", model:"gpt-5.6-sol", displayName:"GPT-5.6 Sol", hidden:false, defaultReasoningEffort:"high", supportedReasoningEfforts:[], inputModalities:["text","image"], supportsPersonality:true, isDefault:true },
      { id:"gpt-5.6-terra", model:"gpt-5.6-terra", displayName:"GPT-5.6 Terra", hidden:false, defaultReasoningEffort:"medium", supportedReasoningEfforts:[], inputModalities:["text","image"], supportsPersonality:true, isDefault:false },
    ],
  ),
  startCodexThread: (projectId:string,model:string) => nativeOrDemo(
    () => invoke<CodexThreadInfo>("start_codex_thread",{projectId,model}),
    () => ({id:`demo-thread-${projectId}`,sessionId:`demo-thread-${projectId}`,preview:"",ephemeral:false,modelProvider:"openai"}),
  ),
  startCodexTurn: (projectId:string,threadId:string,prompt:string,model:string,effort:string) => nativeOrDemo(
    () => invoke<CodexTurnInfo>("start_codex_turn",{projectId,threadId,prompt,model,effort}),
    () => ({id:`demo-turn-${Date.now()}`,status:"inProgress",items:[]}),
  ),
  interruptCodexTurn: (projectId:string,threadId:string,turnId:string) => invoke<void>("interrupt_codex_turn",{projectId,threadId,turnId}),
};

export function errorMessage(error:unknown):string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return "An unexpected local error occurred";
}

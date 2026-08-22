import { invoke } from "@tauri-apps/api/core";
import type { CodexModelInfo, DoctorReport, Project, RepoInspection, RunDetail, RunSummary, StartRunRequest, VerificationResult } from "../types";
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
  applyChanges: (runId:string) => invoke<void>("apply_changes",{runId}),
  discardRun: (runId:string) => invoke<void>("discard_run",{runId}),
  doctor: () => nativeOrDemo(() => invoke<DoctorReport>("doctor"), () => demoDoctor),
  listCodexModels: () => nativeOrDemo(
    () => invoke<CodexModelInfo[]>("list_codex_models"),
    () => [
      { id:"gpt-5.6-sol", model:"gpt-5.6-sol", displayName:"GPT-5.6 Sol", hidden:false, defaultReasoningEffort:"high", supportedReasoningEfforts:[], inputModalities:["text","image"], supportsPersonality:true, isDefault:true },
      { id:"gpt-5.6-terra", model:"gpt-5.6-terra", displayName:"GPT-5.6 Terra", hidden:false, defaultReasoningEffort:"medium", supportedReasoningEfforts:[], inputModalities:["text","image"], supportsPersonality:true, isDefault:false },
    ],
  ),
};

export function errorMessage(error:unknown):string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return "An unexpected local error occurred";
}

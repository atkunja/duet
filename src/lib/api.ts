import { invoke } from "@tauri-apps/api/core";
import type { DoctorReport, Project, RepoInspection, RunDetail, RunSummary, StartRunRequest } from "../types";

export const api = {
  inspectRepository: (path:string) => invoke<RepoInspection>("inspect_repository",{path}),
  addProject: (path:string) => invoke<Project>("add_project",{path}),
  removeProject: (projectId:string) => invoke<void>("remove_project",{projectId}),
  listProjects: () => invoke<Project[]>("list_projects"),
  startRun: (request:StartRunRequest) => invoke<string>("start_run",{request}),
  cancelRun: (runId:string) => invoke<void>("cancel_run",{runId}),
  listRuns: () => invoke<RunSummary[]>("list_runs"),
  getRun: (runId:string) => invoke<RunDetail>("get_run",{runId}),
  getDiff: (runId:string) => invoke<string>("get_diff",{runId}),
  applyChanges: (runId:string) => invoke<void>("apply_changes",{runId}),
  discardRun: (runId:string) => invoke<void>("discard_run",{runId}),
  doctor: () => invoke<DoctorReport>("doctor"),
};

export function errorMessage(error:unknown):string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return "An unexpected local error occurred";
}

import type { DoctorReport, Project, RunDetail, RunSummary, VerificationResult } from "../types";

export const demoProject: Project = {
  id: "demo-project",
  name: "Duet",
  path: "/Users/demo/Projects/duet",
  language: "TypeScript + Rust",
  buildSystem: "Vite + Cargo",
  testCommand: "npm test && cargo test --manifest-path src-tauri/Cargo.toml",
  benchmarkCommand: "",
  lastUsedAt: new Date().toISOString(),
};

export const demoRuns: RunSummary[] = [];

export function demoRun(runId: string): RunDetail {
  const summary = demoRuns.find(run => run.id === runId);
  if (!summary) throw new Error(`Demo run ${runId} was not found`);
  return { ...summary, stages: [], verification: [], changedFiles: [] };
}

export const demoDoctor: DoctorReport = {
  appDataWritable: true,
  databaseHealthy: true,
  git: { installed: true, authenticated: true, version: "git version 2.50.1", detail: "Available" },
  claude: { installed: true, authenticated: true, version: "2.1.191", detail: "Available" },
  codex: { installed: true, authenticated: true, version: "0.144.1", detail: "Available" },
  os: "Development preview",
};

export function demoVerification(command: string): VerificationResult {
  return {
    name: "Console",
    command,
    success: true,
    exitCode: 0,
    stdout: `Development preview\n$ ${command}\nCommand execution is simulated outside the native Duet shell.`,
    stderr: "",
    durationMs: 84,
    required: false,
  };
}

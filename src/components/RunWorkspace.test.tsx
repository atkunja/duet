import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { RunDetail } from "../types";
import { parseSplitDiff, RunWorkspace } from "./RunWorkspace";

vi.mock("@tauri-apps/plugin-opener", () => ({ openPath: vi.fn(() => Promise.resolve()) }));

const run = (overrides: Partial<RunDetail> = {}): RunDetail => ({
  id: "run-12345678",
  projectId: "project-a",
  projectName: "Alpha",
  task: "Build the export flow",
  status: "completed",
  currentStage: "review",
  createdAt: "2026-08-22T00:00:00Z",
  completedAt: "2026-08-22T00:01:00Z",
  worktreePath: "/tmp/duet/run-12345678/implementation",
  additions: 10,
  deletions: 2,
  stages: [],
  verification: [{
    name: "Tests",
    command: "npm test",
    success: true,
    exitCode: 0,
    stdout: "ok",
    stderr: "",
    durationMs: 100,
    required: true,
  }],
  changedFiles: [],
  ...overrides,
});

const handlers = {
  onStop: vi.fn(),
  onApply: vi.fn(),
  onDiscard: vi.fn(),
  onOpenEditor: vi.fn(),
  onError: vi.fn(),
};

describe("RunWorkspace", () => {
  it("exposes accessible tabs and an explicit apply action for completed runs", () => {
    render(<RunWorkspace run={run()} diff="" liveLogs={[]} {...handlers} />);

    expect(screen.getByRole("tablist", { name: "Run details" })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Summary" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("button", { name: /apply changes/i })).toBeEnabled();
    expect(screen.getByText(/changes remain isolated/i)).toBeInTheDocument();
  });

  it("keeps discarded history inspectable without stale worktree controls", () => {
    render(<RunWorkspace run={run({ worktreePath: undefined, discardedAt: "2026-08-22T01:00:00Z" })} diff="" liveLogs={[]} {...handlers} />);

    expect(screen.getByText(/worktree discarded/i)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /open worktree/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /apply changes/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /discard worktree/i })).not.toBeInTheDocument();
  });

  it("renders the persisted failure reason", () => {
    render(<RunWorkspace run={run({ status: "failed", error: "Verification timed out" })} diff="" liveLogs={[]} {...handlers} />);
    expect(screen.getByText("Verification timed out")).toBeInTheDocument();
  });

  it("pairs removed and added lines in the split diff and keeps unified view available", () => {
    const diff = "diff --git a/a.ts b/a.ts\n@@ -2,2 +2,2 @@\n-old value\n+new value\n shared";
    expect(parseSplitDiff(diff)).toEqual(expect.arrayContaining([
      expect.objectContaining({ oldNumber: 2, newNumber: 2, oldText: "old value", newText: "new value", kind: "changed" }),
      expect.objectContaining({ oldNumber: 3, newNumber: 3, oldText: "shared", newText: "shared", kind: "context" }),
    ]));

    render(<RunWorkspace run={run()} diff={diff} liveLogs={[]} {...handlers} />);
    fireEvent.click(screen.getByRole("tab", { name: "Diff" }));
    expect(screen.getByRole("button", { name: "Split diff" })).toHaveClass("active");
    fireEvent.click(screen.getByRole("button", { name: "Unified diff" }));
    expect(screen.getByText("-old value")).toBeInTheDocument();
  });
});

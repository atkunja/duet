import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { Project } from "../types";
import { TaskComposer } from "./TaskComposer";

const project = (overrides: Partial<Project> = {}): Project => ({
  id: "project-a",
  name: "Alpha",
  path: "/tmp/alpha",
  language: "TypeScript",
  buildSystem: "npm",
  testCommand: "npm test",
  benchmarkCommand: "",
  lastUsedAt: "2026-08-22T00:00:00Z",
  ...overrides,
});

describe("TaskComposer", () => {
  it("requires an objective verification command for click and keyboard submission", () => {
    const onRun = vi.fn();
    render(<TaskComposer project={project({ testCommand: "" })} onRun={onRun} busy={false} />);

    const task = screen.getByRole("textbox", { name: "Software engineering task" });
    fireEvent.change(task, { target: { value: "Add a safe export flow" } });
    expect(screen.getByRole("button", { name: /run duet/i })).toBeDisabled();

    fireEvent.keyDown(task, { key: "Enter", metaKey: true });
    expect(onRun).not.toHaveBeenCalled();

    fireEvent.change(screen.getByRole("textbox", { name: /test or build command/i }), {
      target: { value: "npm test -- --run" },
    });
    fireEvent.keyDown(task, { key: "Enter", metaKey: true });
    expect(onRun).toHaveBeenCalledWith(expect.objectContaining({
      projectId: "project-a",
      task: "Add a safe export flow",
      testCommand: "npm test -- --run",
    }));
  });

  it("preserves a draft across same-project refreshes and resets it on project changes", () => {
    const onRun = vi.fn();
    const { rerender } = render(<TaskComposer project={project()} onRun={onRun} busy={false} />);
    const task = screen.getByRole("textbox", { name: "Software engineering task" });
    fireEvent.change(task, { target: { value: "Draft for Alpha" } });

    rerender(<TaskComposer project={project({ lastUsedAt: "2026-08-22T00:01:00Z" })} onRun={onRun} busy={false} />);
    expect(screen.getByRole("textbox", { name: "Software engineering task" })).toHaveValue("Draft for Alpha");

    rerender(<TaskComposer project={project({ id: "project-b", name: "Beta", testCommand: "cargo test" })} onRun={onRun} busy={false} />);
    expect(screen.getByRole("textbox", { name: "Software engineering task" })).toHaveValue("");
    expect(screen.getByRole("textbox", { name: /test or build command/i })).toHaveValue("cargo test");
  });
});

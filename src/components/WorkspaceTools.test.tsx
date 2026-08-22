import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Project } from "../types";
import { api } from "../lib/api";
import { WorkspaceTools } from "./WorkspaceTools";

vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
vi.mock("../lib/api", () => ({
  api: { runProjectCommand: vi.fn() },
  errorMessage: (error: unknown) => String(error),
  isDevelopmentPreview: false,
  isTauriRuntime: true,
}));

const project: Project = {
  id: "p",
  name: "Duet",
  path: "/tmp/duet",
  language: "TypeScript",
  buildSystem: "npm",
  testCommand: "npm test",
  benchmarkCommand: "",
  lastUsedAt: "2026-08-22T00:00:00Z",
};

afterEach(() => {
  vi.clearAllMocks();
  delete (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
});

describe("WorkspaceTools", () => {
  it("runs a project command and renders its bounded result", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { value: {}, configurable: true });
    vi.mocked(api.runProjectCommand).mockResolvedValue({
      name: "Command console",
      command: "git status --short --branch",
      success: true,
      exitCode: 0,
      stdout: "## main",
      stderr: "",
      durationMs: 12,
      required: false,
    });
    render(<WorkspaceTools project={project} onClose={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "Git status" }));
    await waitFor(() => expect(api.runProjectCommand).toHaveBeenCalledWith("p", "git status --short --branch"));
    expect(await screen.findByText(/## main/)).toBeInTheDocument();
    expect(screen.getByText(/exit 0/)).toBeInTheDocument();
  });

  it("loads a normalized local preview URL", () => {
    render(<WorkspaceTools project={project} onClose={vi.fn()} />);
    fireEvent.click(screen.getByRole("tab", { name: "Preview" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Preview URL" }), { target: { value: "localhost:4173" } });
    fireEvent.click(screen.getByRole("button", { name: "Load preview" }));
    expect(screen.getByTitle("Local application preview")).toHaveAttribute("src", "http://localhost:4173");
  });
});

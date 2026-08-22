import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Project } from "../types";
import { api } from "../lib/api";
import { WorkspaceTools } from "./WorkspaceTools";

const eventState = vi.hoisted(() => ({ listener: undefined as ((event: { payload: { operationId:string;stream:string;chunk:string } }) => void) | undefined }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn().mockImplementation((_event, listener) => { eventState.listener = listener; return Promise.resolve(vi.fn()); }) }));
vi.mock("../lib/api", () => ({
  api: {
    runProjectCommand: vi.fn(),
    cancelProjectCommand: vi.fn().mockResolvedValue(undefined),
    openLocalPreview: vi.fn().mockResolvedValue(undefined),
    listCodexModels: vi.fn().mockResolvedValue([{ id:"sol", model:"sol", displayName:"Sol", hidden:false, defaultReasoningEffort:"low", supportedReasoningEfforts:[{reasoningEffort:"low"}], inputModalities:["text"], supportsPersonality:true, isDefault:true }]),
    startCodexThread: vi.fn().mockResolvedValue({ id:"thread-1", ephemeral:true }),
    startCodexTurn: vi.fn().mockResolvedValue({ id:"turn-1", status:"inProgress", items:[] }),
    interruptCodexTurn: vi.fn().mockResolvedValue(undefined),
  },
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
    await waitFor(() => expect(api.runProjectCommand).toHaveBeenCalledWith("p", "git status --short --branch", expect.any(String)));
    expect(await screen.findByText(/## main/)).toBeInTheDocument();
    expect(screen.getByText(/exit 0/)).toBeInTheDocument();
  });

  it("loads a normalized local preview URL", () => {
    render(<WorkspaceTools project={project} onClose={vi.fn()} />);
    fireEvent.click(screen.getByRole("tab", { name: "Preview" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Preview URL" }), { target: { value: "localhost:4173" } });
    fireEvent.click(screen.getByRole("button", { name: "Load preview" }));
    expect(screen.getByTitle("Local application preview")).toHaveAttribute("src", "http://localhost:4173/");
    expect(screen.getByTitle("Local application preview")).toHaveAttribute("sandbox", expect.not.stringContaining("allow-top-navigation"));
  });

  it("rejects non-local preview origins", () => {
    render(<WorkspaceTools project={project} onClose={vi.fn()} />);
    fireEvent.click(screen.getByRole("tab", { name: "Preview" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Preview URL" }), { target: { value: "https://example.com" } });
    fireEvent.click(screen.getByRole("button", { name: "Load preview" }));
    expect(screen.getByRole("alert")).toHaveTextContent(/must use localhost/i);
    expect(screen.queryByTitle("Local application preview")).not.toBeInTheDocument();
  });

  it("streams native output and stops the active process tree", async () => {
    let finish!: (result: never) => void;
    vi.mocked(api.runProjectCommand).mockReturnValue(new Promise(resolve => { finish = resolve; }));
    render(<WorkspaceTools project={project} onClose={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "Git status" }));
    const operationId = await waitFor(() => {
      const call = vi.mocked(api.runProjectCommand).mock.calls[0];
      expect(call).toBeDefined();
      return call[2];
    });
    await act(async () => eventState.listener?.({ payload: { operationId, stream: "stdout", chunk: "checking files…\n" } }));
    expect(screen.getByText(/checking files/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Stop command" }));
    await waitFor(() => expect(api.cancelProjectCommand).toHaveBeenCalledWith(operationId));
    finish(undefined as never);
  });

  it("cancels an owned command when the tools panel closes", async () => {
    vi.mocked(api.runProjectCommand).mockReturnValue(new Promise(() => {}));
    const { unmount } = render(<WorkspaceTools project={project} onClose={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Git status" }));
    const operationId = await waitFor(() => vi.mocked(api.runProjectCommand).mock.calls[0][2]);
    unmount();
    expect(api.cancelProjectCommand).toHaveBeenCalledWith(operationId);
  });

  it("keeps an active Codex session mounted while visiting another tool", async () => {
    render(<WorkspaceTools project={project} onClose={vi.fn()} />);
    fireEvent.click(screen.getByRole("tab", { name: "Codex" }));
    await waitFor(() => expect(screen.getByRole("combobox", { name: "Assistant model" })).toHaveValue("sol"));
    fireEvent.change(screen.getByRole("textbox", { name: "Message Codex" }), { target: { value: "Inspect the project" } });
    fireEvent.click(screen.getByRole("button", { name: "Send to Codex" }));
    await waitFor(() => expect(api.startCodexTurn).toHaveBeenCalled());
    fireEvent.click(screen.getByRole("tab", { name: "Console" }));
    expect(api.interruptCodexTurn).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("tab", { name: "Codex" }));
    expect(screen.getByText("Inspect the project")).toBeInTheDocument();
  });
});

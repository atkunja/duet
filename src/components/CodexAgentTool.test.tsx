import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { listen } from "@tauri-apps/api/event";
import type { CodexTurnInfo, Project } from "../types";
import { api } from "../lib/api";
import { CodexAgentTool } from "./CodexAgentTool";

const eventState = vi.hoisted(() => ({
  listener: undefined as ((event: { payload: any }) => void) | undefined,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockImplementation((_name, listener) => {
    eventState.listener = listener;
    return Promise.resolve(vi.fn());
  }),
}));

vi.mock("../lib/api", () => ({
  api: {
    listCodexModels: vi.fn(),
    startCodexThread: vi.fn(),
    startCodexTurn: vi.fn(),
    interruptCodexTurn: vi.fn(),
  },
  errorMessage: (error: unknown) => String(error),
  isDevelopmentPreview: false,
  isTauriRuntime: true,
}));

const project: Project = {
  id: "p",
  name: "Duet",
  path: "/tmp/duet",
  language: "Rust",
  buildSystem: "Cargo",
  testCommand: "cargo test",
  benchmarkCommand: "",
  lastUsedAt: "now",
};

const model = {
  id: "sol",
  model: "sol",
  displayName: "Sol",
  hidden: false,
  defaultReasoningEffort: "low",
  supportedReasoningEfforts: [{ reasoningEffort: "low" }],
  inputModalities: ["text"],
  supportsPersonality: true,
  isDefault: true,
};

describe("CodexAgentTool", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    eventState.listener = undefined;
    vi.mocked(api.interruptCodexTurn).mockResolvedValue(undefined);
    vi.mocked(api.listCodexModels).mockResolvedValue([model]);
    vi.mocked(api.startCodexThread).mockResolvedValue({ id: "thread-1", ephemeral: false });
    vi.mocked(api.startCodexTurn).mockResolvedValue({ id: "turn-1", status: "inProgress", items: [] });
  });

  it("starts an owned repository thread and renders streamed App Server text", async () => {
    render(<CodexAgentTool project={project}/>);
    const input = screen.getByRole("textbox", { name: "Message Codex" });
    await waitFor(() => expect(screen.getByRole("combobox", { name: "Assistant model" })).toHaveValue("sol"));
    fireEvent.change(input, { target: { value: "Explain the runtime" } });
    fireEvent.click(screen.getByRole("button", { name: "Send to Codex" }));
    await waitFor(() => expect(api.startCodexTurn).toHaveBeenCalledWith("p", "thread-1", "Explain the runtime", "sol", "low"));
    await act(async () => eventState.listener?.({ payload: { sequence: 1, event: { kind: "notification", method: "item/agentMessage/delta", params: { threadId: "thread-1", turnId: "turn-1", itemId: "answer", delta: "The runtime " } } } }));
    await act(async () => eventState.listener?.({ payload: { sequence: 2, event: { kind: "notification", method: "item/agentMessage/delta", params: { threadId: "thread-1", turnId: "turn-1", itemId: "answer", delta: "is bounded." } } } }));
    expect(screen.getByText("The runtime is bounded.")).toBeInTheDocument();
  });

  it("ignores global notifications before it owns their thread", async () => {
    render(<CodexAgentTool project={project}/>);
    await waitFor(() => expect(eventState.listener).toBeTypeOf("function"));
    await act(async () => eventState.listener?.({ payload: { sequence: 1, event: { kind: "notification", method: "item/agentMessage/delta", params: { threadId: "another-project", itemId: "foreign", delta: "Foreign output" } } } }));
    expect(screen.queryByText("Foreign output")).not.toBeInTheDocument();
  });

  it("interrupts a turn that resolves after the panel unmounts", async () => {
    let resolveTurn!: (turn: CodexTurnInfo) => void;
    vi.mocked(api.startCodexTurn).mockReturnValue(new Promise(resolve => { resolveTurn = resolve; }));
    const view = render(<CodexAgentTool project={project}/>);
    await waitFor(() => expect(screen.getByRole("combobox", { name: "Assistant model" })).toHaveValue("sol"));
    fireEvent.change(screen.getByRole("textbox", { name: "Message Codex" }), { target: { value: "Inspect" } });
    fireEvent.click(screen.getByRole("button", { name: "Send to Codex" }));
    await waitFor(() => expect(api.startCodexTurn).toHaveBeenCalled());
    view.unmount();
    await act(async () => resolveTurn({ id: "late-turn", status: "inProgress", items: [] }));
    await waitFor(() => expect(api.interruptCodexTurn).toHaveBeenCalledWith("p", "thread-1", "late-turn"));
  });

  it("keeps reasoning selectable when model discovery omits effort metadata", async () => {
    vi.mocked(api.listCodexModels).mockResolvedValue([{ ...model, defaultReasoningEffort: undefined, supportedReasoningEfforts: [] }]);
    render(<CodexAgentTool project={project}/>);
    await waitFor(() => expect(screen.getByRole("combobox", { name: "Assistant reasoning" })).toHaveValue("high"));
    expect(screen.getByRole("option", { name: "Extra high" })).toBeInTheDocument();
  });

  it("rebinds the native event stream when setup is retried", async () => {
    vi.mocked(listen).mockRejectedValueOnce(new Error("listener unavailable"));
    render(<CodexAgentTool project={project}/>);
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("listener unavailable"));
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(listen).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(screen.getByText("New repository thread")).toBeInTheDocument());
  });

  it("drops stale thread ownership after the runtime closes", async () => {
    render(<CodexAgentTool project={project}/>);
    await waitFor(() => expect(screen.getByRole("combobox", { name: "Assistant model" })).toHaveValue("sol"));
    fireEvent.change(screen.getByRole("textbox", { name: "Message Codex" }), { target: { value: "First" } });
    fireEvent.click(screen.getByRole("button", { name: "Send to Codex" }));
    await waitFor(() => expect(api.startCodexTurn).toHaveBeenCalledTimes(1));
    await act(async () => eventState.listener?.({ payload: { sequence: 5, event: { kind: "closed" } } }));
    expect(screen.getByRole("alert")).toHaveTextContent("disconnected");
    fireEvent.change(screen.getByRole("textbox", { name: "Message Codex" }), { target: { value: "Second" } });
    fireEvent.click(screen.getByRole("button", { name: "Send to Codex" }));
    await waitFor(() => expect(api.startCodexThread).toHaveBeenCalledTimes(2));
  });
});

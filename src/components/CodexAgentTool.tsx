import { ArrowUp, Bot, RotateCw, ShieldCheck, Square } from "lucide-react";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";
import { api, errorMessage, isDevelopmentPreview, isTauriRuntime } from "../lib/api";
import type { CodexModelInfo, CodexRuntimeEnvelope, Project } from "../types";

interface ChatMessage {
  id: string;
  role: "user" | "assistant";
  text: string;
}

const fallbackEfforts = ["low", "medium", "high", "xhigh"];

export function CodexAgentTool({ project }: { project: Project }) {
  const [models, setModels] = useState<CodexModelInfo[]>([]);
  const [model, setModel] = useState("");
  const [effort, setEffort] = useState("high");
  const [threadId, setThreadId] = useState("");
  const [turnId, setTurnId] = useState("");
  const [prompt, setPrompt] = useState("");
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [running, setRunning] = useState(false);
  const [listenerReady, setListenerReady] = useState(!isTauriRuntime);
  const [listenerAttempt, setListenerAttempt] = useState(0);
  const [modelsLoading, setModelsLoading] = useState(true);
  const [error, setError] = useState("");
  const threadRef = useRef("");
  const turnRef = useRef("");
  const generationRef = useRef(0);
  const mountedRef = useRef(true);
  const endRef = useRef<HTMLDivElement>(null);

  const loadModels = useCallback(async () => {
    setModelsLoading(true);
    setError("");
    try {
      const items = await api.listCodexModels();
      if (!mountedRef.current) return;
      if (!items.length) throw new Error("Codex App Server returned no available models");
      const preferred = items.find(item => item.isDefault) ?? items[0];
      setModels(items);
      setModel(preferred.model);
      setEffort(defaultEffort(preferred));
    } catch (cause) {
      if (mountedRef.current) setError(errorMessage(cause));
    } finally {
      if (mountedRef.current) setModelsLoading(false);
    }
  }, []);

  useEffect(() => {
    mountedRef.current = true;
    void loadModels();
    return () => { mountedRef.current = false; };
  }, [loadModels]);

  useEffect(() => {
    generationRef.current += 1;
    const activeThread = threadRef.current;
    const activeTurn = turnRef.current;
    if (activeThread && activeTurn && isTauriRuntime) {
      void api.interruptCodexTurn(project.id, activeThread, activeTurn).catch(() => {});
    }
    threadRef.current = "";
    turnRef.current = "";
    setThreadId("");
    setTurnId("");
    setMessages([]);
    setRunning(false);
  }, [project.id]);

  useEffect(() => {
    if (!isTauriRuntime) {
      setListenerReady(true);
      return;
    }
    let disposed = false;
    setListenerReady(false);
    const off = listen<CodexRuntimeEnvelope>("duet://codex-event", ({ payload }) => {
      if (disposed) return;
      const event = payload.event;
      if (event.kind === "fatalProtocolError") {
        generationRef.current += 1;
        threadRef.current = "";
        turnRef.current = "";
        setThreadId("");
        setTurnId("");
        setError(event.message);
        setRunning(false);
        return;
      }
      if (event.kind === "closed" || event.kind === "shuttingDown") {
        generationRef.current += 1;
        threadRef.current = "";
        turnRef.current = "";
        setThreadId("");
        setTurnId("");
        setRunning(false);
        if (event.kind === "closed") {
          setError("Codex disconnected. Retry to start a fresh local session.");
        }
        return;
      }
      if (event.kind !== "notification") return;
      const ownedThread = threadRef.current;
      const eventThread = stringValue(event.params.threadId);
      if (!ownedThread || eventThread !== ownedThread) return;
      if (event.method === "item/agentMessage/delta") {
        const itemId = stringValue(event.params.itemId) || turnRef.current || "active";
        appendAssistant(`assistant-${itemId}`, stringValue(event.params.delta));
      } else if (event.method === "item/completed") {
        const item = objectValue(event.params.item);
        if (item?.type === "agentMessage") {
          replaceAssistant(
            `assistant-${stringValue(item.id) || turnRef.current || "active"}`,
            stringValue(item.text),
          );
        }
      } else if (event.method === "turn/completed") {
        const turn = objectValue(event.params.turn);
        const completedId = turn ? stringValue(turn.id) : "";
        if (!turnRef.current || !completedId || completedId === turnRef.current) {
          turnRef.current = "";
          setRunning(false);
          if (turn && stringValue(turn.status) === "failed") {
            setError(stringValue(objectValue(turn.error)?.message) || "Codex turn failed");
          }
        }
      }
    });
    void off.then(() => {
      if (!disposed) setListenerReady(true);
    }).catch(cause => {
      if (!disposed) setError(errorMessage(cause));
    });
    return () => {
      disposed = true;
      setListenerReady(false);
      void off.then(unlisten => unlisten?.()).catch(() => {});
    };
  }, [listenerAttempt]);

  useEffect(() => () => {
    generationRef.current += 1;
    const activeThread = threadRef.current;
    const activeTurn = turnRef.current;
    if (activeThread && activeTurn && isTauriRuntime) {
      void api.interruptCodexTurn(project.id, activeThread, activeTurn).catch(() => {});
    }
  }, [project.id]);

  useEffect(() => {
    if (typeof endRef.current?.scrollIntoView === "function") {
      endRef.current.scrollIntoView({ behavior: "smooth", block: "nearest" });
    }
  }, [messages, running]);

  const appendAssistant = (id: string, delta: string) => {
    if (!delta) return;
    setMessages(value => {
      const index = value.findIndex(message => message.id === id);
      if (index < 0) return [...value, { id, role: "assistant", text: delta }];
      return value.map((message, position) => position === index
        ? { ...message, text: `${message.text}${delta}` }
        : message);
    });
  };

  const replaceAssistant = (id: string, text: string) => {
    if (!text) return;
    setMessages(value => {
      const exact = value.findIndex(message => message.role === "assistant" && message.id === id);
      if (exact >= 0) {
        return value.map((message, index) => index === exact ? { ...message, text } : message);
      }
      let lastAssistant = -1;
      value.forEach((message, index) => { if (message.role === "assistant") lastAssistant = index; });
      if (lastAssistant >= 0) {
        return value.map((message, index) => index === lastAssistant ? { ...message, id, text } : message);
      }
      return [...value, { id, role: "assistant", text }];
    });
  };

  const send = async () => {
    const text = prompt.trim();
    if (!text || running || !model || !listenerReady) return;
    const generation = generationRef.current;
    const isCurrent = () => mountedRef.current && generationRef.current === generation;
    setPrompt("");
    setError("");
    setMessages(value => [...value, { id: `user-${Date.now()}`, role: "user", text }]);
    setRunning(true);
    try {
      let activeThread = threadRef.current;
      if (!activeThread) {
        const thread = await api.startCodexThread(project.id, model);
        if (!isCurrent()) return;
        activeThread = thread.id;
        threadRef.current = activeThread;
        setThreadId(activeThread);
      }
      if (!isCurrent()) return;
      const turn = await api.startCodexTurn(project.id, activeThread, text, model, effort);
      if (!isCurrent()) {
        if (isTauriRuntime) {
          void api.interruptCodexTurn(project.id, activeThread, turn.id).catch(() => {});
        }
        return;
      }
      turnRef.current = turn.id;
      setTurnId(turn.id);
      if (isDevelopmentPreview) {
        appendAssistant(
          `assistant-${turn.id}`,
          "This is the browser development preview. In the native app, Codex streams a repository-aware response here through App Server.",
        );
        setRunning(false);
      }
    } catch (cause) {
      if (isCurrent()) {
        setError(errorMessage(cause));
        setRunning(false);
      }
    }
  };

  const stop = async () => {
    if (!threadRef.current || !turnRef.current) return;
    try {
      await api.interruptCodexTurn(project.id, threadRef.current, turnRef.current);
    } catch (cause) {
      setError(errorMessage(cause));
    }
  };

  const selected = models.find(item => item.model === model);
  const supportedEfforts = selected?.supportedReasoningEfforts.map(item => item.reasoningEffort) ?? [];
  const efforts = supportedEfforts.length ? supportedEfforts : fallbackEfforts;
  const unavailable = modelsLoading || !listenerReady || !model;
  const retrySetup = () => {
    generationRef.current += 1;
    threadRef.current = "";
    turnRef.current = "";
    setThreadId("");
    setTurnId("");
    setRunning(false);
    setError("");
    setListenerAttempt(value => value + 1);
    void loadModels();
  };

  return <section className="codex-agent-tool">
    <header className="codex-agent-bar">
      <div><Bot size={14}/><strong>Codex assistant</strong><span><ShieldCheck size={11}/>Read-only</span></div>
      <div>
        <select aria-label="Assistant model" value={model} disabled={modelsLoading || !models.length} onChange={event => {
          const next = models.find(item => item.model === event.target.value);
          setModel(event.target.value);
          if (next) setEffort(defaultEffort(next));
        }}>{models.map(item => <option key={item.id} value={item.model}>{item.displayName}</option>)}</select>
        <select aria-label="Assistant reasoning" value={effort} disabled={!model} onChange={event => setEffort(event.target.value)}>
          {efforts.map(value => <option key={value} value={value}>{title(value)}</option>)}
        </select>
      </div>
    </header>
    <div className="codex-chat-log" role="log" aria-live="polite" aria-relevant="additions text" aria-atomic="false">
      {messages.length ? messages.map(message => <article key={message.id} className={message.role}>
        <span>{message.role === "assistant" ? "CX" : "You"}</span><p>{message.text}</p>
      </article>) : <div className="codex-chat-empty"><Bot/><strong>Ask Codex about this project</strong><p>Explore the codebase, plan changes, or investigate a bug. This assistant is sandboxed read-only; start a Duet run to modify files.</p></div>}
      {running && <div className="codex-thinking" role="status"><i/><i/><i/>Codex is working…</div>}
      <div ref={endRef}/>
    </div>
    {error && <div className="codex-setup-error" role="alert"><span>{error}</span>{!running && <button onClick={retrySetup}><RotateCw size={11}/>Retry</button>}</div>}
    <div className="codex-chat-input">
      <textarea aria-label="Message Codex" value={prompt} onChange={event => setPrompt(event.target.value)} placeholder={modelsLoading ? "Loading Codex models…" : listenerReady ? "Ask about the repository…" : "Connecting to Codex…"} onKeyDown={event => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void send(); } }}/>
      {running ? <button aria-label="Stop Codex" onClick={stop} disabled={!isTauriRuntime}><Square size={12} fill="currentColor"/></button> : <button aria-label="Send to Codex" onClick={send} disabled={!prompt.trim() || unavailable}><ArrowUp size={14}/></button>}
    </div>
    <footer>{threadId ? <span>Thread {threadId.slice(0, 12)}{turnId ? ` · Turn ${turnId.slice(0, 10)}` : ""}</span> : <span>{listenerReady ? "New repository thread" : "Connecting event stream…"}</span>}</footer>
  </section>;
}

function defaultEffort(model: CodexModelInfo): string {
  return model.defaultReasoningEffort
    ?? model.supportedReasoningEfforts[0]?.reasoningEffort
    ?? "high";
}

function stringValue(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function objectValue(value: unknown): Record<string, unknown> | undefined {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

function title(value: string): string {
  return value === "xhigh" ? "Extra high" : value.charAt(0).toUpperCase() + value.slice(1);
}

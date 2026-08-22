import { ExternalLink, Globe2, Play, RotateCw, Square, TerminalSquare, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { listen } from "@tauri-apps/api/event";
import type { ConsoleOutputEvent, Project, VerificationResult } from "../types";
import { api, errorMessage, isDevelopmentPreview, isTauriRuntime } from "../lib/api";

type ToolTab = "console" | "preview";

export function WorkspaceTools({ project, onClose }: { project: Project; onClose: () => void }) {
  const [tab, setTab] = useState<ToolTab>("console");
  const [command, setCommand] = useState(project.testCommand || "git status --short");
  const [result, setResult] = useState<VerificationResult>();
  const [liveOutput, setLiveOutput] = useState("");
  const [running, setRunning] = useState(false);
  const [error, setError] = useState("");
  const [url, setUrl] = useState("http://127.0.0.1:3000");
  const [loadedUrl, setLoadedUrl] = useState("");
  const [reloadKey, setReloadKey] = useState(0);
  const outputRef = useRef<HTMLPreElement>(null);
  const operationRef = useRef("");
  const native = isTauriRuntime;
  const canRun = native || isDevelopmentPreview;
  const output = useMemo(() => result ? [result.stdout, result.stderr].filter(Boolean).join("\n") : "", [result]);

  useEffect(() => {
    if (!native) return;
    let disposed = false;
    const off = listen<ConsoleOutputEvent>("duet://console-output", ({ payload }) => {
      if (disposed || payload.operationId !== operationRef.current) return;
      setLiveOutput(value => `${value}${payload.chunk}`.slice(-1_000_000));
      requestAnimationFrame(() => outputRef.current?.scrollTo({ top: outputRef.current.scrollHeight }));
    });
    return () => { disposed = true; void off.then(unlisten => unlisten()); };
  }, [native]);

  const run = async (nextCommand = command) => {
    if (!nextCommand.trim() || running || !canRun) return;
    setCommand(nextCommand);
    setRunning(true);
    setError("");
    setResult(undefined);
    setLiveOutput("");
    const operationId = crypto.randomUUID();
    operationRef.current = operationId;
    try {
      const next = await api.runProjectCommand(project.id, nextCommand.trim(), operationId);
      setResult(next);
      requestAnimationFrame(() => outputRef.current?.scrollTo({ top: outputRef.current.scrollHeight }));
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setRunning(false);
    }
  };

  const stop = async () => {
    if (!native || !operationRef.current) return;
    try { await api.cancelProjectCommand(operationRef.current); }
    catch (cause) { setError(errorMessage(cause)); }
  };

  const openPreview = () => {
    const next = normalizeLocalUrl(url);
    setUrl(next);
    setLoadedUrl(next);
  };

  return <aside className="workspace-tools" aria-label="Workspace tools">
    <header className="workspace-tools-head">
      <div role="tablist" aria-label="Workspace tools">
        <button role="tab" aria-selected={tab === "console"} className={tab === "console" ? "active" : ""} onClick={() => setTab("console")}><TerminalSquare size={14}/>Console</button>
        <button role="tab" aria-selected={tab === "preview"} className={tab === "preview" ? "active" : ""} onClick={() => setTab("preview")}><Globe2 size={14}/>Preview</button>
      </div>
      <button className="tool-close" aria-label="Close workspace tools" onClick={onClose}><X size={15}/></button>
    </header>

    {tab === "console" ? <section className="console-tool" role="tabpanel">
      <div className="console-actions">
        <button onClick={() => run("git status --short --branch")} disabled={running || !canRun}>Git status</button>
        {project.testCommand && <button onClick={() => run(project.testCommand)} disabled={running || !canRun}>Run tests</button>}
      </div>
      <pre ref={outputRef} aria-live="polite">{running ? `$ ${command}\n\n${liveOutput || "Running…"}` : result ? `$ ${result.command}\n\n${output || "Command completed without output."}\n\n[exit ${result.exitCode ?? "—"} · ${result.durationMs}ms]` : canRun ? "Run a project command without leaving Duet." : "The command console is available in the native Duet app."}</pre>
      {error && <p className="console-error" role="alert">{error}</p>}
      <div className="console-input">
        <span>$</span><input aria-label="Project command" value={command} onChange={event => setCommand(event.target.value)} onKeyDown={event => { if (event.key === "Enter") run(); }} disabled={running}/>
        {running && native ? <button aria-label="Stop command" onClick={stop}><Square size={12} fill="currentColor"/></button> : <button aria-label="Run command" onClick={() => run()} disabled={!command.trim() || running || !canRun}><Play size={13} fill="currentColor"/></button>}
      </div>
    </section> : <section className="preview-tool" role="tabpanel">
      <div className="preview-bar">
        <input aria-label="Preview URL" value={url} onChange={event => setUrl(event.target.value)} onKeyDown={event => { if (event.key === "Enter") openPreview(); }}/>
        <button aria-label="Load preview" onClick={openPreview}><Play size={13}/></button>
        <button aria-label="Reload preview" onClick={() => setReloadKey(value => value + 1)} disabled={!loadedUrl}><RotateCw size={13}/></button>
        <button aria-label="Open preview externally" disabled={!loadedUrl} onClick={() => loadedUrl && (native ? openUrl(loadedUrl) : window.open(loadedUrl, "_blank", "noopener"))}><ExternalLink size={13}/></button>
      </div>
      {loadedUrl ? <iframe key={`${loadedUrl}-${reloadKey}`} title="Local application preview" src={loadedUrl}/> : <div className="preview-empty"><Globe2/><strong>Preview a local app</strong><p>Start its development server in Console, then load the localhost URL here.</p></div>}
    </section>}
  </aside>;
}

function normalizeLocalUrl(value: string) {
  const trimmed = value.trim();
  if (!trimmed) return "http://127.0.0.1:3000";
  if (/^https?:\/\//i.test(trimmed)) return trimmed;
  return `http://${trimmed}`;
}

import { ArrowUp, Bot, ShieldCheck, Square, X } from "lucide-react";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef, useState } from "react";
import { api, errorMessage, isDevelopmentPreview, isTauriRuntime } from "../lib/api";
import type { CodexModelInfo, CodexRuntimeEnvelope, Project } from "../types";

interface ChatMessage { id:string; role:"user"|"assistant"; text:string }
interface PendingRequest { token:string; method:string }

export function CodexAgentTool({project}:{project:Project}) {
  const [models,setModels]=useState<CodexModelInfo[]>([]);
  const [model,setModel]=useState("");
  const [effort,setEffort]=useState("high");
  const [threadId,setThreadId]=useState("");
  const [turnId,setTurnId]=useState("");
  const [prompt,setPrompt]=useState("");
  const [messages,setMessages]=useState<ChatMessage[]>([]);
  const [pending,setPending]=useState<PendingRequest>();
  const [running,setRunning]=useState(false);
  const [error,setError]=useState("");
  const threadRef=useRef("");const turnRef=useRef("");const endRef=useRef<HTMLDivElement>(null);

  useEffect(()=>{let disposed=false;api.listCodexModels().then(items=>{if(disposed||!items.length)return;setModels(items);const preferred=items.find(item=>item.isDefault)??items[0];setModel(preferred.model);setEffort(preferred.defaultReasoningEffort??preferred.supportedReasoningEfforts[0]?.reasoningEffort??"high")}).catch(cause=>{if(!disposed)setError(errorMessage(cause))});return()=>{disposed=true}},[]);
  useEffect(()=>{if(!isTauriRuntime)return;let disposed=false;const off=listen<CodexRuntimeEnvelope>("duet://codex-event",({payload})=>{if(disposed)return;const event=payload.event;if(event.kind==="serverRequest"){setPending({token:event.token,method:event.method});return}if(event.kind==="serverRequestResolved"){setPending(value=>value?.token===event.token?undefined:value);return}if(event.kind==="fatalProtocolError"){setError(event.message);setRunning(false);return}if(event.kind!=="notification")return;const eventThread=stringValue(event.params.threadId);if(threadRef.current&&eventThread&&eventThread!==threadRef.current)return;if(event.method==="item/agentMessage/delta"){const itemId=stringValue(event.params.itemId)||turnRef.current||"active";appendAssistant(`assistant-${itemId}`,stringValue(event.params.delta));return}if(event.method==="item/completed"){const item=objectValue(event.params.item);if(item&&item.type==="agentMessage")replaceAssistant(`assistant-${stringValue(item.id)||turnRef.current||"active"}`,stringValue(item.text));return}if(event.method==="turn/completed"){const turn=objectValue(event.params.turn);const completedId=turn?stringValue(turn.id):"";if(!turnRef.current||!completedId||completedId===turnRef.current){turnRef.current="";setRunning(false);if(turn&&stringValue(turn.status)==="failed")setError(stringValue(objectValue(turn.error)?.message)||"Codex turn failed")}}}).catch(cause=>{if(!disposed)setError(errorMessage(cause));return undefined});return()=>{disposed=true;const activeThread=threadRef.current,activeTurn=turnRef.current;if(activeThread&&activeTurn)void api.interruptCodexTurn(activeThread,activeTurn).catch(()=>{});void off.then(unlisten=>unlisten?.())}},[]);
  useEffect(()=>{if(typeof endRef.current?.scrollIntoView==="function")endRef.current.scrollIntoView({behavior:"smooth",block:"nearest"})},[messages,running]);

  const appendAssistant=(id:string,delta:string)=>{if(!delta)return;setMessages(value=>{const index=value.findIndex(message=>message.id===id);if(index<0)return [...value,{id,role:"assistant",text:delta}];return value.map((message,position)=>position===index?{...message,text:`${message.text}${delta}`}:message)})};
  const replaceAssistant=(id:string,text:string)=>{if(!text)return;setMessages(value=>{const assistantIndex=value.findIndex(message=>message.role==="assistant"&&message.id===id);if(assistantIndex<0){let last=-1;value.forEach((message,index)=>{if(message.role==="assistant")last=index});if(last>=0)return value.map((message,index)=>index===last?{...message,id,text}:message);return [...value,{id,role:"assistant",text}]}return value.map((message,index)=>index===assistantIndex?{...message,text}:message)})};
  const send=async()=>{const text=prompt.trim();if(!text||running||!model)return;setPrompt("");setError("");setMessages(value=>[...value,{id:`user-${Date.now()}`,role:"user",text}]);setRunning(true);try{let activeThread=threadRef.current;if(!activeThread){const thread=await api.startCodexThread(project.id,model);activeThread=thread.id;threadRef.current=activeThread;setThreadId(activeThread)}const turn=await api.startCodexTurn(project.id,activeThread,text,model,effort);turnRef.current=turn.id;setTurnId(turn.id);if(isDevelopmentPreview){appendAssistant(`assistant-${turn.id}`,"This is the browser development preview. In the native app, Codex streams a repository-aware response here through App Server.");setRunning(false)}}catch(cause){setError(errorMessage(cause));setRunning(false)}};
  const stop=async()=>{if(!threadRef.current||!turnRef.current)return;try{await api.interruptCodexTurn(threadRef.current,turnRef.current)}catch(cause){setError(errorMessage(cause))}};
  const decline=async()=>{if(!pending)return;try{await api.rejectCodexRequest(pending.token);setPending(undefined)}catch(cause){setError(errorMessage(cause))}};
  const selected=models.find(item=>item.model===model);const efforts=selected?.supportedReasoningEfforts.map(item=>item.reasoningEffort)??["low","medium","high","xhigh"];

  return <section className="codex-agent-tool" role="tabpanel">
    <header className="codex-agent-bar"><div><Bot size={14}/><strong>Codex assistant</strong><span><ShieldCheck size={11}/>Read-only</span></div><div><select aria-label="Assistant model" value={model} onChange={event=>{const next=models.find(item=>item.model===event.target.value);setModel(event.target.value);setEffort(next?.defaultReasoningEffort??next?.supportedReasoningEfforts[0]?.reasoningEffort??effort)}}>{models.map(item=><option key={item.id} value={item.model}>{item.displayName}</option>)}</select><select aria-label="Assistant reasoning" value={effort} onChange={event=>setEffort(event.target.value)}>{efforts.map(value=><option key={value} value={value}>{title(value)}</option>)}</select></div></header>
    <div className="codex-chat-log">{messages.length?messages.map(message=><article key={message.id} className={message.role}><span>{message.role==="assistant"?"CX":"You"}</span><p>{message.text}</p></article>):<div className="codex-chat-empty"><Bot/><strong>Ask Codex about this project</strong><p>Explore the codebase, plan changes, or investigate a bug. This assistant is sandboxed read-only; start a Duet run to modify files.</p></div>}{running&&<div className="codex-thinking"><i/><i/><i/>Codex is working…</div>}<div ref={endRef}/></div>
    {pending&&<div className="codex-request" role="alert"><div><strong>Codex requested an unsupported action</strong><small>{pending.method}</small></div><button onClick={decline}><X size={12}/>Decline</button></div>}
    {error&&<p className="console-error" role="alert">{error}</p>}
    <div className="codex-chat-input"><textarea aria-label="Message Codex" value={prompt} onChange={event=>setPrompt(event.target.value)} placeholder="Ask about the repository…" onKeyDown={event=>{if(event.key==="Enter"&&!event.shiftKey){event.preventDefault();void send()}}}/>{running?<button aria-label="Stop Codex" onClick={stop} disabled={!isTauriRuntime}><Square size={12} fill="currentColor"/></button>:<button aria-label="Send to Codex" onClick={send} disabled={!prompt.trim()||!model}><ArrowUp size={14}/></button>}</div>
    <footer>{threadId?<span>Thread {threadId.slice(0,12)}{turnId?` · Turn ${turnId.slice(0,10)}`:""}</span>:<span>New repository thread</span>}</footer>
  </section>;
}

function stringValue(value:unknown):string{return typeof value==="string"?value:""}
function objectValue(value:unknown):Record<string,unknown>|undefined{return value&&typeof value==="object"&&!Array.isArray(value)?value as Record<string,unknown>:undefined}
function title(value:string):string{return value==="xhigh"?"Extra high":value.charAt(0).toUpperCase()+value.slice(1)}

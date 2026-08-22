import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import { FolderGit2, Plus, ShieldCheck } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { Sidebar } from "./components/Sidebar";
import { TaskComposer } from "./components/TaskComposer";
import { RunWorkspace } from "./components/RunWorkspace";
import { Settings } from "./components/Settings";
import { api, errorMessage } from "./lib/api";
import type { Project, RunDetail, RunEvent, RunSummary, StartRunRequest } from "./types";

export default function App(){
  const [projects,setProjects]=useState<Project[]>([]);const [runs,setRuns]=useState<RunSummary[]>([]);const [selectedProject,setSelectedProject]=useState<string>();const [selectedRun,setSelectedRun]=useState<string>();const [detail,setDetail]=useState<RunDetail>();const [diff,setDiff]=useState("");const [logs,setLogs]=useState<Record<string,string[]>>({});const [screen,setScreen]=useState<"workspace"|"settings">("workspace");const [error,setError]=useState("");const [busy,setBusy]=useState("");
  const refresh=useCallback(async()=>{try{const [p,r]=await Promise.all([api.listProjects(),api.listRuns()]);setProjects(p);setRuns(r);if(!selectedProject&&p.length)setSelectedProject(p[0].id)}catch(e){setError(errorMessage(e))}},[selectedProject]);
  const loadRun=useCallback(async(id:string)=>{try{const [d,patch]=await Promise.all([api.getRun(id),api.getDiff(id).catch(()=>"")]);setDetail(d);setDiff(patch)}catch(e){setError(errorMessage(e))}},[]);
  useEffect(()=>{refresh()},[refresh]); useEffect(()=>{if(selectedRun)loadRun(selectedRun)},[selectedRun,loadRun]);
  useEffect(()=>{let disposed=false;const off=listen<RunEvent>("duet://run-event",async({payload})=>{if(disposed)return;if(payload.type==="agentOutput")setLogs(v=>({...v,[payload.runId]:[...(v[payload.runId]||[]).slice(-499),`[${payload.stage}] ${payload.line}`]}));if(payload.runId===selectedRun)loadRun(payload.runId);if(["runCompleted","runFailed","runCancelled"].includes(payload.type)){refresh();notify(payload.type==="runCompleted"?"Duet completed your task.":"Duet needs your attention.")}else if(payload.type!=="agentOutput")refresh()});return()=>{disposed=true;off.then(fn=>fn())}},[selectedRun,loadRun,refresh]);
  const addProject=async()=>{try{const path=await open({directory:true,multiple:false,title:"Choose a Git repository"});if(typeof path!=="string")return;setBusy("project");const project=await api.addProject(path);await refresh();setSelectedProject(project.id);setSelectedRun(undefined);setScreen("workspace")}catch(e){setError(errorMessage(e))}finally{setBusy("")}};
  const startRun=async(request:StartRunRequest)=>{try{setBusy("run");const id=await api.startRun(request);setSelectedRun(id);await refresh();await loadRun(id)}catch(e){setError(errorMessage(e))}finally{setBusy("")}};
  const apply=async()=>{if(!selectedRun||!window.confirm("Apply this run's patch to the original repository? Duet will abort if the target changed or is dirty."))return;try{setBusy("apply");await api.applyChanges(selectedRun);window.alert("Changes applied to the original working tree. Nothing was committed or pushed.")}catch(e){setError(errorMessage(e))}finally{setBusy("")}};
  const discard=async()=>{if(!selectedRun||!window.confirm("Discard this Duet worktree and its temporary branch? This cannot be undone."))return;try{setBusy("discard");await api.discardRun(selectedRun);setSelectedRun(undefined);setDetail(undefined);await refresh()}catch(e){setError(errorMessage(e))}finally{setBusy("")}};
  const project=projects.find(p=>p.id===selectedProject);
  return <div className="app-shell"><Sidebar projects={projects} runs={runs} selectedProject={selectedProject} selectedRun={selectedRun} onProject={id=>{setSelectedProject(id);setSelectedRun(undefined);setScreen("workspace")}} onRun={id=>{setSelectedRun(id);setSelectedProject(runs.find(r=>r.id===id)?.projectId);setScreen("workspace")}} onAdd={addProject} onSettings={()=>setScreen("settings")}/>
    <div className="main-shell">{screen==="settings"?<Settings onBack={()=>setScreen("workspace")}/>:selectedRun&&detail?<RunWorkspace run={detail} diff={diff} liveLogs={logs[selectedRun]||[]} onStop={()=>api.cancelRun(selectedRun).catch(e=>setError(errorMessage(e)))} onApply={apply} onDiscard={discard} busyAction={busy}/>:project?<TaskComposer project={project} onRun={startRun} busy={busy==="run"}/>:<Welcome onAdd={addProject} busy={busy==="project"}/>}</div>
    {error&&<div className="toast error"><button onClick={()=>setError("")}>×</button><strong>Duet couldn’t continue</strong><span>{error}</span></div>}
  </div>
}

function Welcome({onAdd,busy}:{onAdd:()=>void;busy:boolean}){return <main className="welcome"><div className="welcome-mark"><span>D</span></div><span className="eyebrow">LOCAL AI ENGINEERING</span><h1>Two agents. One verified result.</h1><p>Connect a Git repository to let Codex architect and review while Claude builds and repairs—entirely inside an isolated worktree.</p><button className="primary-button" onClick={onAdd} disabled={busy}><Plus size={15}/>{busy?"Inspecting…":"Add your first project"}</button><div className="welcome-points"><span><FolderGit2/>Worktree isolated</span><span><ShieldCheck/>Objectively verified</span></div></main>}
async function notify(body:string){try{let granted=await isPermissionGranted();if(!granted)granted=(await requestPermission())==="granted";if(granted)sendNotification({title:"Duet",body})}catch{/* Notifications are optional. */}}

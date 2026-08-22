import { ArrowRight, ChevronDown, FlaskConical, GitBranch, PanelRight, Play, Settings2, ShieldCheck, Sparkles } from "lucide-react";
import { useEffect, useState } from "react";
import type { Project, StartRunRequest } from "../types";
import { api } from "../lib/api";
import { AgentModeSelector, defaultAgentModeSelection, type ModelOption, type ReasoningEffort } from "./AgentModeSelector";
import { WorkspaceTools } from "./WorkspaceTools";

export function TaskComposer({project,onRun,busy}:{project:Project;onRun:(request:StartRunRequest)=>void;busy:boolean}){
  const [task,setTask]=useState(""); const [test,setTest]=useState(project.testCommand); const [benchmark,setBenchmark]=useState(project.benchmarkCommand); const [repairs,setRepairs]=useState(3); const [mock,setMock]=useState(false); const [toolsOpen,setToolsOpen]=useState(false); const [agents,setAgents]=useState(defaultAgentModeSelection);
  const [codexModels,setCodexModels]=useState<ModelOption[]>();
  useEffect(()=>{setTask("");setTest(project.testCommand);setBenchmark(project.benchmarkCommand)},[project.id]);
  useEffect(()=>{let disposed=false;api.listCodexModels().then(models=>{if(disposed||!models.length)return;const options=models.map(model=>({value:model.model,label:model.displayName,defaultReasoning:reasoningEffort(model.defaultReasoningEffort),reasoningEfforts:model.supportedReasoningEfforts.map(option=>reasoningEffort(option.reasoningEffort)).filter((effort):effort is ReasoningEffort=>Boolean(effort))}));setCodexModels(options);const preferred=models.find(model=>model.isDefault)??models[0];const option=options.find(value=>value.value===preferred.model);setAgents(value=>({...value,codex:{model:preferred.model,reasoning:option?.defaultReasoning??(option?.reasoningEfforts?.includes(value.codex.reasoning)?value.codex.reasoning:option?.reasoningEfforts?.[0]??value.codex.reasoning)}}))}).catch(()=>{/* Static model options remain available if App Server is unavailable. */});return()=>{disposed=true}},[]);
  const submit=()=>{if(task.trim()&&test.trim())onRun({projectId:project.id,task:task.trim(),testCommand:test,benchmarkCommand:benchmark||undefined,maxRepairs:repairs,mockAgents:mock,agentMode:agents.mode,executionLocation:agents.location,codexModel:agents.codex.model,claudeModel:agents.claude.model,codexReasoning:agents.codex.reasoning,claudeReasoning:agents.claude.reasoning})};
  return <main className="composer-page">
    <header className="topbar"><div><span className="eyebrow">NEW TASK</span><h1>{project.name}</h1></div><div className="topbar-actions"><div className="repo-meta"><GitBranch size={14}/><span>{project.language}</span><i/> <span>{project.buildSystem}</span></div><button className={`secondary-button ${toolsOpen?"selected":""}`} aria-pressed={toolsOpen} onClick={()=>setToolsOpen(value=>!value)}><PanelRight size={14}/>Tools</button></div></header>
    <div className={`composer-wrap ${toolsOpen?"tools-open":""}`}>
      <div className="composer-main">
        <div className="composer-heading"><span className="duet-symbol"><Sparkles size={22}/></span><h2>What do you want {modeName(agents.mode)} to build?</h2><p>{modeDescription(agents.mode)}</p></div>
        <div className="task-card">
        <textarea aria-label="Software engineering task" autoFocus value={task} onChange={e=>setTask(e.target.value)} placeholder="Describe a feature, bug fix, refactor, or performance goal…" onKeyDown={e=>{if((e.metaKey||e.ctrlKey)&&e.key==="Enter")submit()}}/>
        <details className="agent-settings">
          <summary><span><Settings2 size={13}/><strong>{modeName(agents.mode)}</strong><small>{agents.location === "local" ? "Local" : "Cloud"}</small></span><span>Models &amp; routing<ChevronDown size={13}/></span></summary>
          <AgentModeSelector value={agents} onChange={setAgents} disabled={busy} codexModels={codexModels}/>
        </details>
        <WorkflowStrip mode={agents.mode}/>
        <div className="form-grid">
          <label><span>Test or build command <em>required</em></span><input required aria-required="true" value={test} onChange={e=>setTest(e.target.value)} placeholder="Required objective verification command"/></label>
          <label><span>Benchmark <em>optional</em></span><input value={benchmark} onChange={e=>setBenchmark(e.target.value)} placeholder="e.g. cargo bench"/></label>
          <label><span>Repair rounds</span><select value={repairs} onChange={e=>setRepairs(Number(e.target.value))}><option>1</option><option>2</option><option>3</option><option>4</option><option>5</option></select></label>
        </div>
        <div className="composer-footer"><label className="check-row"><input type="checkbox" checked={mock} onChange={e=>setMock(e.target.checked)}/><FlaskConical size={15}/><span>Mock agents</span><small>No Claude/Codex usage</small></label><button className="primary-button" disabled={!task.trim()||!test.trim()||busy} onClick={submit}><Play size={15} fill="currentColor"/>{busy?"Starting…":`Run ${modeName(agents.mode)}`}<kbd>⌘ ↵</kbd></button></div>
        </div>
        <div className="privacy-note"><ShieldCheck size={15}/><span>Duet stores data locally and delegates only through your configured official CLIs, which connect to their respective services.</span></div>
      </div>
      {toolsOpen&&<WorkspaceTools key={project.id} project={project} onClose={()=>setToolsOpen(false)}/>}
    </div>
  </main>
}

function modeName(mode:"duet"|"codex"|"claude"){return mode==="duet"?"Duet":mode==="codex"?"Codex":"Claude"}
function modeDescription(mode:"duet"|"codex"|"claude"){return mode==="duet"?"Codex plans and reviews. Claude implements and repairs. Duet verifies the result.":mode==="codex"?"Codex carries the task from architecture through implementation and review.":"Claude carries the task from architecture through implementation and review."}
function reasoningEffort(value:string|undefined):ReasoningEffort|undefined{return value&&["minimal","low","medium","high","xhigh","max","ultra"].includes(value)?value as ReasoningEffort:undefined}
function WorkflowStrip({mode}:{mode:"duet"|"codex"|"claude"}){const planner=mode==="claude"?"claude":"codex";const builder=mode==="codex"?"codex":"claude";return <div className="workflow-strip"><AgentDot agent={planner}/><b>Plan</b><ArrowRight/><AgentDot agent={builder}/><b>Build</b><ArrowRight/><span className="verify-dot"><ShieldCheck/></span><b>Verify</b><ArrowRight/><AgentDot agent={planner}/><b>Review</b><ArrowRight/><AgentDot agent={builder}/><b>Repair</b></div>}
function AgentDot({agent}:{agent:"codex"|"claude"}){return <span className={`agent ${agent}`}>{agent==="codex"?"CX":"CL"}</span>}

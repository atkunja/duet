import { ArrowRight, FlaskConical, GitBranch, Play, ShieldCheck, Sparkles } from "lucide-react";
import { useEffect, useState } from "react";
import type { Project, StartRunRequest } from "../types";

export function TaskComposer({project,onRun,busy}:{project:Project;onRun:(request:StartRunRequest)=>void;busy:boolean}){
  const [task,setTask]=useState(""); const [test,setTest]=useState(project.testCommand); const [benchmark,setBenchmark]=useState(project.benchmarkCommand); const [repairs,setRepairs]=useState(3); const [mock,setMock]=useState(false);
  useEffect(()=>{setTask("");setTest(project.testCommand);setBenchmark(project.benchmarkCommand)},[project.id]);
  const submit=()=>{if(task.trim()&&test.trim())onRun({projectId:project.id,task:task.trim(),testCommand:test,benchmarkCommand:benchmark||undefined,maxRepairs:repairs,mockAgents:mock})};
  return <main className="composer-page">
    <header className="topbar"><div><span className="eyebrow">NEW TASK</span><h1>{project.name}</h1></div><div className="repo-meta"><GitBranch size={14}/><span>{project.language}</span><i/> <span>{project.buildSystem}</span></div></header>
    <div className="composer-wrap">
      <div className="composer-heading"><span className="duet-symbol"><Sparkles size={22}/></span><h2>What do you want Duet to build?</h2><p>Codex plans and reviews. Claude implements and repairs. Duet verifies the result.</p></div>
      <div className="task-card">
        <textarea aria-label="Software engineering task" autoFocus value={task} onChange={e=>setTask(e.target.value)} placeholder="Describe a feature, bug fix, refactor, or performance goal…" onKeyDown={e=>{if((e.metaKey||e.ctrlKey)&&e.key==="Enter")submit()}}/>
        <div className="workflow-strip"><span className="agent codex">CX</span><b>Architect</b><ArrowRight/><span className="agent claude">CL</span><b>Build</b><ArrowRight/><span className="verify-dot"><ShieldCheck/></span><b>Verify</b><ArrowRight/><span className="agent codex">CX</span><b>Review</b><ArrowRight/><span className="agent claude">CL</span><b>Repair</b></div>
        <div className="form-grid">
          <label><span>Test or build command <em>required</em></span><input required aria-required="true" value={test} onChange={e=>setTest(e.target.value)} placeholder="Required objective verification command"/></label>
          <label><span>Benchmark <em>optional</em></span><input value={benchmark} onChange={e=>setBenchmark(e.target.value)} placeholder="e.g. cargo bench"/></label>
          <label><span>Repair rounds</span><select value={repairs} onChange={e=>setRepairs(Number(e.target.value))}><option>1</option><option>2</option><option>3</option><option>4</option><option>5</option></select></label>
        </div>
        <div className="composer-footer"><label className="check-row"><input type="checkbox" checked={mock} onChange={e=>setMock(e.target.checked)}/><FlaskConical size={15}/><span>Mock agents</span><small>No Claude/Codex usage</small></label><button className="primary-button" disabled={!task.trim()||!test.trim()||busy} onClick={submit}><Play size={15} fill="currentColor"/>{busy?"Starting…":"Run Duet"}<kbd>⌘ ↵</kbd></button></div>
      </div>
      <div className="privacy-note"><ShieldCheck size={15}/><span>Duet stores data locally and delegates only through your configured official CLIs, which connect to their respective services.</span></div>
    </div>
  </main>
}

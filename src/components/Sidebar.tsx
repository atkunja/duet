import { FolderGit2, History, Plus, Settings2 } from "lucide-react";
import type { Project, RunSummary } from "../types";
import { relativeTime, shortId } from "../lib/format";
import { StatusBadge } from "./Status";

interface Props {projects:Project[];runs:RunSummary[];selectedProject?:string;selectedRun?:string;onProject:(id:string)=>void;onRun:(id:string)=>void;onAdd:()=>void;onSettings:()=>void}
export function Sidebar({projects,runs,selectedProject,selectedRun,onProject,onRun,onAdd,onSettings}:Props){
  return <aside className="sidebar">
    <div className="traffic-spacer"/>
    <div className="brand"><span className="brand-mark">D</span><span>Duet</span><span className="local-pill">LOCAL</span></div>
    <nav className="side-scroll">
      <section className="nav-section">
        <div className="section-label"><span>Projects</span><button className="icon-button tiny" onClick={onAdd} title="Add project"><Plus size={14}/></button></div>
        {projects.length===0?<button className="empty-nav" onClick={onAdd}><Plus size={15}/> Add a repository</button>:projects.map(project=><button key={project.id} className={`project-row ${selectedProject===project.id&&!selectedRun?"active":""}`} onClick={()=>onProject(project.id)}>
          <FolderGit2 size={15}/><span><strong>{project.name}</strong><small>{project.language}</small></span>
        </button>)}
      </section>
      <section className="nav-section runs-section">
        <div className="section-label"><span>Recent runs</span><History size={13}/></div>
        {runs.slice(0,12).map(run=><button key={run.id} className={`run-row ${selectedRun===run.id?"active":""}`} onClick={()=>onRun(run.id)}>
          <div><span className="run-number">#{shortId(run.id).slice(0,4)}</span><StatusBadge status={run.status}/></div>
          <strong>{run.task}</strong><small>{run.projectName} · {relativeTime(run.createdAt)}</small>
        </button>)}
      </section>
    </nav>
    <button className="settings-row" onClick={onSettings}><Settings2 size={16}/> Settings & Doctor</button>
  </aside>
}

import { AlertTriangle, Check, Circle, LoaderCircle, OctagonX } from "lucide-react";

export function StatusIcon({status,size=16}:{status:string;size?:number}) {
  if(["completed","pass","passed"].includes(status))return <span className="status-icon success"><Check size={size}/></span>;
  if(["failed","fail"].includes(status))return <span className="status-icon danger"><OctagonX size={size}/></span>;
  if(["running","working"].includes(status))return <span className="status-icon running"><LoaderCircle size={size}/></span>;
  if(["interrupted","cancelled"].includes(status))return <span className="status-icon warning"><AlertTriangle size={size}/></span>;
  return <span className="status-icon muted"><Circle size={Math.max(8,size-5)}/></span>;
}

export function StatusBadge({status}:{status:string}) { return <span className={`status-badge ${status}`}><span/>{status}</span>; }

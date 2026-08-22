export function relativeTime(value:string):string {
  const seconds=Math.round((Date.now()-new Date(value).getTime())/1000);
  if(seconds<10)return "just now"; if(seconds<60)return `${seconds}s ago`; const minutes=Math.floor(seconds/60);
  if(minutes<60)return `${minutes}m ago`; const hours=Math.floor(minutes/60); if(hours<24)return `${hours}h ago`;
  return `${Math.floor(hours/24)}d ago`;
}
export function duration(ms?:number):string { if(ms==null)return ""; const s=Math.round(ms/1000); return s<60?`${s}s`:`${Math.floor(s/60)}m ${s%60}s`; }
export function shortId(id:string):string { return id.slice(0,8).toUpperCase(); }
export function titleCase(value:string):string { return value.replace(/-/g," ").replace(/\b\w/g,c=>c.toUpperCase()); }

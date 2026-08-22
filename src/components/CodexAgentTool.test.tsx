import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { Project } from "../types";
import { api } from "../lib/api";
import { CodexAgentTool } from "./CodexAgentTool";

const eventState = vi.hoisted(() => ({ listener: undefined as ((event:{payload:any})=>void)|undefined }));
vi.mock("@tauri-apps/api/event",()=>({listen:vi.fn().mockImplementation((_name,listener)=>{eventState.listener=listener;return Promise.resolve(vi.fn())})}));
vi.mock("../lib/api",()=>({
  api:{
    listCodexModels:vi.fn(),startCodexThread:vi.fn(),startCodexTurn:vi.fn(),interruptCodexTurn:vi.fn(),rejectCodexRequest:vi.fn(),
  },
  errorMessage:(error:unknown)=>String(error),isDevelopmentPreview:false,isTauriRuntime:true,
}));

const project:Project={id:"p",name:"Duet",path:"/tmp/duet",language:"Rust",buildSystem:"Cargo",testCommand:"cargo test",benchmarkCommand:"",lastUsedAt:"now"};

describe("CodexAgentTool",()=>{
  it("starts a repository thread and renders streamed App Server text",async()=>{
    vi.mocked(api.interruptCodexTurn).mockResolvedValue(undefined);
    vi.mocked(api.listCodexModels).mockResolvedValue([{id:"sol",model:"sol",displayName:"Sol",hidden:false,defaultReasoningEffort:"low",supportedReasoningEfforts:[{reasoningEffort:"low"}],inputModalities:["text"],supportsPersonality:true,isDefault:true}]);
    vi.mocked(api.startCodexThread).mockResolvedValue({id:"thread-1",ephemeral:false});
    vi.mocked(api.startCodexTurn).mockResolvedValue({id:"turn-1",status:"inProgress",items:[]});
    render(<CodexAgentTool project={project}/>);
    const input=screen.getByRole("textbox",{name:"Message Codex"});
    await waitFor(()=>expect(screen.getByRole("combobox",{name:"Assistant model"})).toHaveValue("sol"));
    fireEvent.change(input,{target:{value:"Explain the runtime"}});fireEvent.click(screen.getByRole("button",{name:"Send to Codex"}));
    await waitFor(()=>expect(api.startCodexTurn).toHaveBeenCalledWith("p","thread-1","Explain the runtime","sol","low"));
    await act(async()=>eventState.listener?.({payload:{sequence:1,event:{kind:"notification",method:"item/agentMessage/delta",params:{threadId:"thread-1",turnId:"turn-1",itemId:"answer",delta:"The runtime "}}}}));
    await act(async()=>eventState.listener?.({payload:{sequence:2,event:{kind:"notification",method:"item/agentMessage/delta",params:{threadId:"thread-1",turnId:"turn-1",itemId:"answer",delta:"is bounded."}}}}));
    expect(screen.getByText("The runtime is bounded.")).toBeInTheDocument();
  });

  it("surfaces and safely declines server requests",async()=>{
    vi.mocked(api.listCodexModels).mockResolvedValue([]);
    vi.mocked(api.rejectCodexRequest).mockResolvedValue(undefined);
    render(<CodexAgentTool project={project}/>);
    await act(async()=>eventState.listener?.({payload:{sequence:3,event:{kind:"serverRequest",token:"opaque",method:"item/tool/requestUserInput",params:{}}}}));
    expect(screen.getByRole("alert")).toHaveTextContent(/unsupported action/i);
    fireEvent.click(screen.getByRole("button",{name:/decline/i}));
    await waitFor(()=>expect(api.rejectCodexRequest).toHaveBeenCalledWith("opaque"));
  });
});

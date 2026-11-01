'use strict';
// Conversation state is independent of the currently visible conversation.
const viewSessions=new Map();
let navigationVersion=0,workspaceStreamStarted=false,workspaceConnecting=null;
const sessionDefaults=()=>({busy:false,run:null,runMode:null,streamText:'',streamThinking:'',streamTools:[],approvals:[],notice:'',scrollFollow:true,uploading:0,pending:[],sending:false,startedAt:0,submitting:false,cancelRequested:false,draft:'',submission:null,chat:null});
const visibleConversation=()=>state.chat?.id||'new';
function draftStorageKey(id){return 'gemma.draft.v1.'+id;}
function readDraft(id){
  const raw=localStorage.getItem(draftStorageKey(id));
  if(!raw)return null;
  try{return JSON.parse(raw);}catch{
    try{localStorage.setItem(draftStorageKey(id)+'.corrupt',raw);}catch{}
    return null;
  }
}
function currentSession(id=visibleConversation()){
  if(!viewSessions.has(id)){
    const session=sessionDefaults(),draft=readDraft(id);
    if(draft){session.draft=typeof draft.text==='string'?draft.text:'';session.pending=Array.isArray(draft.attachments)?draft.attachments:[];session.submission=draft.submission||null;session.draftMode=draft.mode;}
    viewSessions.set(id,session);
  }
  return viewSessions.get(id);
}
function setupSessionState(){
  for(const key of Object.keys(sessionDefaults()).filter(key=>key!=='chat')){
    Object.defineProperty(state,key,{configurable:true,get(){return currentSession()[key];},set(value){currentSession()[key]=value;}});
  }
}
function persistDraft(id=visibleConversation(),session=currentSession(id)){
  try{
    const value={text:session.draft,attachments:session.pending,submission:session.submission,mode:id===visibleConversation()?state.mode:session.draftMode};
    if(!value.text&&!value.attachments.length&&!value.submission)localStorage.removeItem(draftStorageKey(id));
    else localStorage.setItem(draftStorageKey(id),JSON.stringify(value));
    session.draftError=false;return true;
  }catch(error){if(!session.draftError){toast('Не удалось сохранить черновик на этом устройстве: '+error.message,true);session.draftError=true;}return false;}
}
function captureDraft(){
  const session=currentSession();session.draft=document.getElementById('prompt')?.value||'';session.draftMode=state.mode;persistDraft();
}
function restoreDraft(){
  const session=currentSession();document.getElementById('prompt').value=session.draft;
  if(session.draftMode&&modes[session.draftMode]&&!session.busy)setMode(session.draftMode);
  renderPending();resizeInput();
}
function prunePreviousSession(id){const session=viewSessions.get(id);if(session&&!session.busy&&!session.uploading&&!session.sending&&!session.draftError)viewSessions.delete(id);}
function clearAcceptedDraft(session,id,payload){
  if(payload.retry){session.submission=null;persistDraft(id,session);return;}
  const submitted=payload.submittedDraft??payload.content;
  if(session.draft===submitted||session.draft.trim()===submitted){session.draft='';if(visibleConversation()===id)document.getElementById('prompt').value='';}
  const ids=new Set(payload.attachments||[]);session.pending=session.pending.filter(a=>!ids.has(a.id));session.submission=null;persistDraft(id,session);
  if(visibleConversation()===id){resizeInput();renderPending();}
}
function restoreWorkspaceRun(run){
  const session=currentSession(run.conversation);Object.assign(session,run.state||{});
  session.busy=true;session.run=run.id;session.runMode=run.input.mode;session.startedAt=Date.parse(run.created)||Date.now();session.draftMode=run.input.mode;
  if(session.submission?.request_id===run.id)clearAcceptedDraft(session,run.conversation,session.submission);
}
async function connectWorkspace(){
  if(workspaceConnecting)return workspaceConnecting;
  workspaceConnecting=initializeWorkspace();
  try{await workspaceConnecting;}finally{workspaceConnecting=null;}
}
async function initializeWorkspace(){
  if(workspaceStreamStarted)return;
  const snapshot=await api('/api/session');
  for(const run of snapshot.runs)restoreWorkspaceRun(run);
  const selected=currentSession();
  if(selected.submission&&!snapshot.runs.some(run=>run.id===selected.submission.request_id)){
    try{const run=await api('/api/runs/'+selected.submission.request_id);if(run.conversation===visibleConversation())clearAcceptedDraft(selected,visibleConversation(),selected.submission);}catch(error){if(error.status!==404)throw error;}
  }
  workspaceStreamStarted=true;
  if(state.busy){renderMessages();updateSend();startResponseTimer();}
  renderChats();
  consumeEvents('/api/events?after='+snapshot.cursor,undefined,async event=>{
    if(!event.conversation_id){if(event.type==='error')toast(event.message,true);return;}
    const id=event.conversation_id,session=currentSession(id);
    await handleRunEvent(event,false,session,id);
  },{persistent:true,cursor:snapshot.cursor}).catch(error=>{workspaceStreamStarted=false;toast('Соединение с задачами потеряно: '+error.message,true);});
}
async function finishConversationRun(session,id){
  session.busy=false;session.run=null;session.submitting=false;session.approvals=[];
  if(!session.chat)try{session.chat=await api('/api/chats/'+id);}catch(error){toast(error.message,true);}
  if(visibleConversation()===id){clearTimeout(renderTimer);renderTimer=null;paintStream();if(session.chat)state.chat=session.chat;renderCompletedConversation();updateSend();}
  session.streamText='';session.streamThinking='';session.streamTools=[];session.chat=null;
  if(visibleConversation()!==id)prunePreviousSession(id);
  stopResponseTimer();await refreshChats();
}
async function switchConversation(id){
  if(state.sending)return;
  const previous=visibleConversation(),version=++navigationVersion;captureDraft();
  const chat=await api('/api/chats/'+id);if(version!==navigationVersion)return;
  const session=currentSession(id);state.chat=session.busy&&session.chat?session.chat:chat;
  if(session.busy)session.chat=state.chat;
  localStorage.setItem('gemma.lastChat',id);
  const mode=session.busy?session.runMode:state.chat.mode;setMode(modes[mode]?mode:'agent');
  document.getElementById('chat-title').textContent=state.chat.title;
  restoreDraft();renderChats();renderMessages();updateSend();followScroll(true);
  if(session.busy)startResponseTimer();else stopResponseTimer();
  if(previous!==id)prunePreviousSession(previous);
  if(innerWidth<761)setSidebarOpen(false);
}
function newConversation(){
  if(state.sending)return;
  const previous=visibleConversation();captureDraft();navigationVersion++;state.chat=null;
  localStorage.removeItem('gemma.lastChat');document.getElementById('chat-title').textContent='Новый чат';
  restoreDraft();renderChats();renderMessages();updateSend();stopResponseTimer();document.getElementById('prompt').focus();
  if(previous!=='new')prunePreviousSession(previous);
  if(innerWidth<761){setSidebarOpen(false);document.getElementById('prompt').focus();}
}
async function submitMessage(retry=false){
  if(state.sending||state.uploading)return;if(state.busy)return stopRun();
  const raw=document.getElementById('prompt').value;
  let content=raw.trim(),attachments=[...state.pending];
  if(retry){const previous=[...(state.chat?.messages||[])].reverse().find(m=>m.role==='user');if(!previous)return;content=previous.content;attachments=previous.attachments||[];}
  if(!content&&!attachments.length)return;
  captureDraft();const session=currentSession();const previousSubmission=session.submission;session.sending=true;
  let id=visibleConversation();
  try{
    if(!state.chat){
      const chat=await api('/api/chats',{method:'POST',body:{mode:state.mode}});
      viewSessions.set(chat.id,session);viewSessions.delete('new');state.chat=chat;session.chat=chat;id=chat.id;
      if(!state.chats.some(c=>c.id===id))state.chats.unshift(chat);
      if(persistDraft(id,session))localStorage.removeItem(draftStorageKey('new'));
    }
    await connectWorkspace();
    localStorage.setItem('gemma.lastChat',id);
    const payload={conversation:id,content,attachments:attachments.map(a=>a.id),mode:state.mode,source:document.getElementById('source').value,target:document.getElementById('target').value,retry,thinking:thinkingChoice,web:webChoice};
    const previous=previousSubmission;
    const comparable=value=>JSON.stringify(Object.fromEntries(Object.entries(value).filter(([key])=>!['request_id','submittedDraft'].includes(key))));
    payload.request_id=previous&&comparable(previous)===comparable(payload)?previous.request_id:crypto.randomUUID();
    session.submission={...payload,submittedDraft:raw};persistDraft(id,session);
    if(retry){if(state.chat.messages.at(-1)?.role==='assistant')state.chat.messages.pop();if(state.chat.messages.at(-1)?.role==='user')state.chat.messages.pop();}
    if(!state.chat.messages.some(message=>message.id===payload.request_id))state.chat.messages.push({id:payload.request_id,role:'user',content,attachments,tools:[],created:new Date().toISOString()});session.chat=state.chat;
    Object.assign(session,{busy:true,run:payload.request_id,runMode:payload.mode,streamText:'',streamThinking:'',streamTools:[],approvals:[],notice:'Отправка запроса…',scrollFollow:true,startedAt:Date.now(),submitting:true,cancelRequested:false});
    renderMessages();updateSend();startResponseTimer();
    try{
      const accepted=await api('/api/runs',{method:'POST',body:payload});
      if(!retry)clearAcceptedDraft(session,id,{...payload,submittedDraft:raw});else{session.submission=null;persistDraft(id,session);}
      if(session.cancelRequested)await api('/api/stop/'+payload.request_id,{method:'POST',body:{}});
      if(!['running','queued','awaiting_approval','stopping'].includes(accepted.status)&&session.busy){session.chat=await api('/api/chats/'+id);await finishConversationRun(session,id);}
    }catch(error){
      let accepted=false;
      if(!error.status)try{const run=await api('/api/runs/'+payload.request_id);accepted=true;if(!['running','queued','awaiting_approval','stopping'].includes(run.status))await finishConversationRun(session,id);}catch{}
      if(!accepted){session.busy=false;session.run=null;try{session.chat=await api('/api/chats/'+id);}catch{}if(visibleConversation()===id){state.chat=session.chat||state.chat;renderMessages();}await refreshChats();}
      toast(error.message,true);
    }
  }catch(error){toast(error.message,true);}
  finally{session.sending=false;session.submitting=false;if(visibleConversation()===id)updateSend();stopResponseTimer();}
}

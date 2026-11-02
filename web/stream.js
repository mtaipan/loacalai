'use strict';
function renderApprovalShortcut(){
  let button=document.getElementById('approval-shortcut');const pending=state.approvals||[];
  if(!pending.length){button?.remove();return;}
  if(!button){button=document.createElement('button');button.id='approval-shortcut';button.type='button';button.className='button approval-shortcut';document.querySelector('.mode-row').before(button);
    button.onclick=()=>{const card=[...document.querySelectorAll('[data-approval-card]')].find(card=>card.dataset.approvalCard===button.dataset.target);if(!card)return;const control=card.querySelector('select')||card.querySelector('[data-approval-preview]')||card.querySelector('[data-approve]');(control||card).scrollIntoView({block:'center',inline:'nearest'});control?.focus({preventScroll:true});};
  }
  button.dataset.target=pending[0].id;button.textContent=pending.length===1?'Перейти к подтверждению':'Перейти к подтверждениям · '+pending.length;
}
let streamRenderCost=0,pendingViewRender=null;
function selectionInside(element){const selection=getSelection();return !!element&&!!selection&&!selection.isCollapsed&&element.contains(selection.anchorNode)&&element.contains(selection.focusNode);}
function interactionInside(element){return selectionInside(element)||!!element&&element.contains(document.activeElement)&&document.activeElement.matches('button,input,select,summary,a,[tabindex]');}
function primeStreamCards(){
  const body=document.getElementById('stream-body');if(body){body._text=state.streamText;[...body.childNodes].forEach(node=>node._source=node.outerHTML??node.textContent);}
  for(const[selector,key]of [['#stream-tools','data-tool-key'],['#approvals','data-approval-card']]){
    const container=document.querySelector(selector);if(!container)continue;
    [...container.children].forEach((node,index)=>{if(!node.hasAttribute(key))node.setAttribute(key,String(index));node._source=node.outerHTML;});
  }
}
function syncStreamContent(container,text){
  if(container._text===text||selectionInside(container))return;
  const template=document.createElement('template');template.innerHTML=markdown(text)+'<span class="cursor"></span>';
  let index=0;
  for(const fresh of [...template.content.childNodes]){
    const source=fresh.outerHTML??fresh.textContent,old=container.childNodes[index];
    if(old?._source!==source){fresh._source=source;if(old)old.replaceWith(fresh);else container.append(fresh);}
    index++;
  }
  while(container.childNodes.length>index)container.lastChild.remove();
  container._text=text;enhanceMarkdown(container);
}
function paintStream(){
  const body=document.getElementById('stream-body');if(!body)return;
  const began=performance.now();syncStreamContent(body,state.streamText);
  syncCards(document.getElementById('stream-tools'),toolsHtml(state.streamTools),'data-tool-key');
  const reason=document.getElementById('stream-reasoning');if(reason){reason.hidden=!state.streamThinking;const pre=reason.querySelector('pre'),text=state.streamThinking||'';if(pre._text!==text&&!selectionInside(pre)){if(pre.firstChild?.nodeType===Node.TEXT_NODE&&pre.childNodes.length===1&&text.startsWith(pre._text||''))pre.firstChild.appendData(text.slice((pre._text||'').length));else pre.textContent=text;pre._text=text;}}
  syncCards(document.getElementById('approvals'),state.approvals.map(approvalHtml).join(''),'data-approval-card');
  renderApprovalShortcut();
  const history=document.getElementById('stream-history');if(history){const html=runHistoryLinks(state.run,currentSession());if(history._source!==html){history.innerHTML=html;history._source=html;}}
  const notice=document.querySelector('#stream-notice span:last-child');if(notice)notice.textContent=state.notice||'Подготовка модели';
  followScroll();streamRenderCost=performance.now()-began;
}
function renderCompletedConversation(){
  const current=document.getElementById('conversation');
  current?.querySelectorAll('[data-approval-card] button,[data-approval-card] select').forEach(control=>control.disabled=true);
  if(interactionInside(current)){
    pendingViewRender=visibleConversation();current.querySelector('.cursor')?.remove();
    const notice=current.querySelector('#stream-notice');if(notice)notice.textContent='Ответ получен';
  }else{pendingViewRender=null;renderMessages();}
}
function setupStreamSelection(){
  const update=()=>{
    const conversation=document.getElementById('conversation');if(interactionInside(conversation))return;
    if(pendingViewRender===visibleConversation()){pendingViewRender=null;renderMessages();}
    else if(state.busy)scheduleStream();
  };
  uiListen(document,'selectionchange',update);
  uiListen(document,'focusout',()=>queueMicrotask(update));
}

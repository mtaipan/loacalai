'use strict';
// Model and document HTML is content, never application UI. Keep language
// hints for code fences without admitting application CSS classes or actions.
DOMPurify.addHook('uponSanitizeAttribute',(node,attribute)=>{
  if(attribute.attrName==='class')attribute.keepAttr=node.nodeName==='CODE'&&attribute.attrValue.length<=80&&/^language-[a-z0-9_+#.-]+$/i.test(attribute.attrValue);
});
function sanitizeContent(html){
  return DOMPurify.sanitize(html,{
    ALLOWED_TAGS:['p','br','hr','h1','h2','h3','h4','h5','h6','ul','ol','li','pre','code','blockquote','a','strong','b','em','i','s','del','table','thead','tbody','tfoot','tr','th','td','dl','dt','dd','details','summary','div','span','sub','sup','mark','kbd','img'],
    ALLOWED_ATTR:['href','title','src','alt','colspan','rowspan','start','reversed','open','class'],
    ALLOW_DATA_ATTR:false,ALLOW_ARIA_ATTR:false,
    FORBID_ATTR:['style','id','name','autofocus','tabindex','contenteditable']
  });
}

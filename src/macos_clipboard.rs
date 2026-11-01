//! Preserve every materializable pasteboard item/type without a sentinel write.
use anyhow::{ensure, Context, Result};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardWriting, NSWorkspace};
use objc2_foundation::NSArray;

pub fn revision() -> isize { NSPasteboard::generalPasteboard().changeCount() }
pub fn foreground() -> Option<i32> {
    NSWorkspace::sharedWorkspace().frontmostApplication().map(|app|app.processIdentifier())
}
pub struct Snapshot { items: Vec<Retained<NSPasteboardItem>>, pub revision:isize }
impl Snapshot {
    pub fn capture() -> Result<Self> {
        let board=NSPasteboard::generalPasteboard();
        let revision=board.changeCount();
        let mut items=vec![];
        if let Some(original)=board.pasteboardItems() {
            for item in original.iter() {
                let copy=NSPasteboardItem::new();
                for kind in item.types().iter() {
                    let data=item.dataForType(&kind).context("Специальный формат буфера недоступен. Используйте ручное копирование и перевод буфера; буфер не изменён")?;
                    ensure!(copy.setData_forType(&data,&kind),"Не удалось сохранить формат буфера; буфер не изменён");
                }
                items.push(copy);
            }
        }
        ensure!(board.changeCount()==revision,"Буфер изменился во время чтения. Повторите перевод выделения");
        Ok(Self{items,revision})
    }
    pub fn restore(&self, expected:isize) -> Result<bool> {
        let board=NSPasteboard::generalPasteboard();
        // AppKit has no cross-process compare-and-swap. Check ownership immediately
        // before writing; do not overwrite a later observed user copy.
        if board.changeCount()!=expected {return Ok(false);}
        let items:Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>>=self.items.iter().map(|item|ProtocolObject::from_retained(item.clone())).collect();
        let objects=NSArray::from_retained_slice(&items);
        board.clearContents();
        ensure!(items.is_empty()||board.writeObjects(&objects),"Не удалось восстановить буфер обмена");
        Ok(true)
    }
}

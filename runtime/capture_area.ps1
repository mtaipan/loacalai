param([Parameter(Mandatory=$true)][string]$OutputPath)
$ErrorActionPreference='Stop'
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
Add-Type -AssemblyName System.Windows.Forms,System.Drawing
Add-Type -ReferencedAssemblies System.Windows.Forms.dll,System.Drawing.dll -TypeDefinition @'
using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
using System.Windows.Forms;
public class LocalAreaPicker : Form {
 [DllImport("user32.dll")] static extern bool SetProcessDpiAwarenessContext(IntPtr value);
 [DllImport("user32.dll")] static extern bool SetProcessDPIAware();
 Bitmap source; string output; Point anchor; Rectangle selected; bool dragging;
 public static void Dpi(){try{if(!SetProcessDpiAwarenessContext(new IntPtr(-4)))SetProcessDPIAware();}catch{SetProcessDPIAware();}}
 public LocalAreaPicker(Bitmap bitmap,Rectangle bounds,string path){
  source=bitmap;output=path;Text="Local Gemma Studio - select screen region";
  FormBorderStyle=FormBorderStyle.None;StartPosition=FormStartPosition.Manual;
  AutoScaleMode=AutoScaleMode.None;Bounds=bounds;TopMost=true;ShowInTaskbar=false;
  Cursor=Cursors.Cross;KeyPreview=true;DoubleBuffered=true;
 }
 protected override void OnShown(EventArgs e){base.OnShown(e);Activate();Focus();}
 protected override void OnPaint(PaintEventArgs e){
  e.Graphics.DrawImageUnscaled(source,0,0);
  using(var dark=new SolidBrush(Color.FromArgb(155,0,0,0)))using(var mask=new Region(ClientRectangle)){
   if(selected.Width>0&&selected.Height>0)mask.Exclude(selected);e.Graphics.FillRegion(dark,mask);
  }
  if(selected.Width>0&&selected.Height>0)using(var pen=new Pen(Color.FromArgb(234,191,145),2))e.Graphics.DrawRectangle(pen,selected);
  using(var font=new Font("Segoe UI",13,FontStyle.Bold))using(var brush=new SolidBrush(Color.White))e.Graphics.DrawString("Выделите область для перевода. Esc - отмена",font,brush,20,20);
 }
 protected override void OnMouseDown(MouseEventArgs e){if(e.Button==MouseButtons.Left){anchor=e.Location;dragging=true;Capture=true;selected=Rectangle.Empty;}}
 protected override void OnMouseMove(MouseEventArgs e){if(dragging){selected=Rectangle.Intersect(ClientRectangle,Rectangle.FromLTRB(Math.Min(anchor.X,e.X),Math.Min(anchor.Y,e.Y),Math.Max(anchor.X,e.X),Math.Max(anchor.Y,e.Y)));Invalidate();}}
 protected override void OnMouseUp(MouseEventArgs e){
  if(!dragging)return;OnMouseMove(e);dragging=false;Capture=false;
  if(selected.Width<8||selected.Height<8){selected=Rectangle.Empty;Invalidate();return;}
  using(var crop=source.Clone(selected,PixelFormat.Format32bppArgb)){crop.Save(output,ImageFormat.Png);}
  DialogResult=DialogResult.OK;Close();
 }
 protected override void OnKeyDown(KeyEventArgs e){if(e.KeyCode==Keys.Escape){DialogResult=DialogResult.Cancel;Close();}base.OnKeyDown(e);}
}
'@
[LocalAreaPicker]::Dpi()
$taskBounds=[Windows.Forms.SystemInformation]::VirtualScreen
$taskBitmap=[Drawing.Bitmap]::new($taskBounds.Width,$taskBounds.Height)
$taskGraphics=[Drawing.Graphics]::FromImage($taskBitmap)
try{
 $taskGraphics.CopyFromScreen($taskBounds.Location,[Drawing.Point]::Empty,$taskBounds.Size)
 $taskPicker=[LocalAreaPicker]::new($taskBitmap,$taskBounds,$OutputPath)
 try{$taskResult=$taskPicker.ShowDialog();@{cancelled=($taskResult -ne [Windows.Forms.DialogResult]::OK)} | ConvertTo-Json -Compress}finally{$taskPicker.Dispose()}
}finally{$taskGraphics.Dispose();$taskBitmap.Dispose()}

using System;
using System.IO;
using System.IO.Compression;
using System.Reflection;
using System.Diagnostics;
using System.Drawing;
using System.Windows.Forms;
class Setup : Form {
 TextBox folder=new TextBox(), log=new TextBox(); Button install=new Button();
 public Setup(){Text="Local Gemma Studio — установка";Size=new Size(660,420);StartPosition=FormStartPosition.CenterScreen;BackColor=Color.FromArgb(30,30,29);ForeColor=Color.White;Font=new Font("Segoe UI",10);
 var title=new Label{Text="Local Gemma Studio 0.12.1\nМодели скачиваются отдельно. Выберите папку для приложения и всех его данных.",Left=22,Top=20,Width=600,Height=60};Controls.Add(title);
 folder.SetBounds(22,94,490,28);folder.Text=@"A:\LocalGemmaStudio";Controls.Add(folder);
 var browse=new Button{Text="Папка…",Left=520,Top=92,Width=100};browse.Click+=(s,e)=>{using(var dialog=new FolderBrowserDialog()){dialog.Description="Папка приложения и моделей";if(dialog.ShowDialog()==DialogResult.OK)folder.Text=dialog.SelectedPath;}};Controls.Add(browse);
 install.Text="Установить и запустить";install.SetBounds(22,140,598,38);install.Click+=Install;Controls.Add(install);
 log.Multiline=true;log.ReadOnly=true;log.ScrollBars=ScrollBars.Vertical;log.SetBounds(22,195,598,160);log.BackColor=BackColor;log.ForeColor=ForeColor;Controls.Add(log);
 }
 public static void Extract(string destination){destination=Path.GetFullPath(destination);Directory.CreateDirectory(destination);using(var input=Assembly.GetExecutingAssembly().GetManifestResourceStream("payload.zip"))using(var archive=new ZipArchive(input,ZipArchiveMode.Read)){foreach(var entry in archive.Entries){string path=Path.GetFullPath(Path.Combine(destination,entry.FullName));if(!path.StartsWith(destination.TrimEnd(Path.DirectorySeparatorChar)+Path.DirectorySeparatorChar,StringComparison.OrdinalIgnoreCase))throw new Exception("Invalid archive path");Directory.CreateDirectory(Path.GetDirectoryName(path));using(var src=entry.Open())using(var dst=File.Create(path))src.CopyTo(dst);}}}
 async void Install(object sender,EventArgs e){install.Enabled=false;folder.Enabled=false;try{string root=Path.GetFullPath(folder.Text);if(!Directory.Exists(Path.GetPathRoot(root)))throw new Exception("Диск не найден. Выберите существующий диск.");log.Text="Распаковка…";await System.Threading.Tasks.Task.Run(()=>Extract(root));
 var start=new ProcessStartInfo("powershell.exe","-NoProfile -ExecutionPolicy Bypass -File \""+Path.Combine(root,"scripts","provision-windows.ps1")+"\" -TaskRoot \""+root+"\""){UseShellExecute=false,CreateNoWindow=true,RedirectStandardOutput=true,RedirectStandardError=true};
 log.AppendText("\r\nПодготовка локальных компонентов. Первый запуск требует интернета.\r\n");using(var process=new Process{StartInfo=start}){process.OutputDataReceived+=(s,a)=>{if(a.Data!=null)BeginInvoke((Action)(()=>log.AppendText(a.Data+"\r\n")));};process.ErrorDataReceived+=(s,a)=>{if(a.Data!=null)BeginInvoke((Action)(()=>log.AppendText(a.Data+"\r\n")));};process.Start();process.BeginOutputReadLine();process.BeginErrorReadLine();await System.Threading.Tasks.Task.Run(()=>process.WaitForExit());if(process.ExitCode!=0)throw new Exception("Подготовка не завершена. Подробности выше; повторите установку.");}
 Process.Start(new ProcessStartInfo(Path.Combine(root,"LocalGemmaStudio.exe")){WorkingDirectory=root});Close();
 }catch(Exception error){log.AppendText("\r\n"+error.Message);install.Enabled=true;folder.Enabled=true;}}
 [STAThread]static void Main(string[] args){if(args.Length==2&&args[0]=="--extract-only"){Extract(args[1]);return;}Application.EnableVisualStyles();Application.Run(new Setup());}
}

using System;
using System.IO;
using System.IO.Compression;
using System.Reflection;
using System.Diagnostics;
using System.Drawing;
using System.Linq;
using System.Collections.Generic;
using System.Web.Script.Serialization;
using System.Windows.Forms;
class Setup : Form {
 TextBox folder=new TextBox(),log=new TextBox(),description=new TextBox(),modelFolder=new TextBox();
 Button install=new Button(),browse=new Button(),modelBrowse=new Button(); ListView models=new ListView();
 JavaScriptSerializer json=new JavaScriptSerializer(); bool running=false;
 public Setup(){
  Text="Local Gemma Studio — офлайн-установка";Size=new Size(900,800);MinimumSize=Size;StartPosition=FormStartPosition.CenterScreen;BackColor=Color.FromArgb(30,30,29);ForeColor=Color.White;Font=new Font("Segoe UI",10);
  Controls.Add(new Label{Text="Local Gemma Studio\nКомпоненты устанавливаются без интернета. Необязательная загрузка моделей требует сети.",Left=22,Top=18,Width=840,Height=58});
  folder.SetBounds(22,84,715,28);folder.Text=Directory.Exists(@"A:\")?@"A:\LocalGemmaStudio":Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),"LocalGemmaStudio");Controls.Add(folder);
  browse.Text="Папка…";browse.SetBounds(748,82,110,30);browse.Click+=(s,e)=>PickFolder(folder);Controls.Add(browse);
  Controls.Add(new Label{Text="Выберите модели или пропустите этот шаг. По умолчанию ничего не выбрано.",Left=22,Top=124,Width=840,Height=26});
  models.SetBounds(22,154,836,285);models.View=View.Details;models.CheckBoxes=true;models.FullRowSelect=true;models.MultiSelect=false;models.BackColor=BackColor;models.ForeColor=ForeColor;models.Columns.Add("Модель",480);models.Columns.Add("Размер весов",130);models.Columns.Add("Назначение",190);Controls.Add(models);
  Dictionary<string,object>[] catalog;using(var reader=new StreamReader(Assembly.GetExecutingAssembly().GetManifestResourceStream("catalog.json")))catalog=json.Deserialize<Dictionary<string,object>[]>(reader.ReadToEnd());
  var groups=new Dictionary<string,ListViewGroup>();
  foreach(var m in catalog.OrderByDescending(x=>x.ContainsKey("recommended")&&(bool)x["recommended"])){
   string category=m.ContainsKey("recommended")&&(bool)m["recommended"]?"Рекомендуемые":Get(m,"category");if(category=="Translation")category="Перевод";if(category=="Chat and agent")category="Чат и агент";
   if(!groups.ContainsKey(category)){groups[category]=new ListViewGroup(category);models.Groups.Add(groups[category]);}
   var row=new ListViewItem(Get(m,"name"),groups[category]);row.SubItems.Add(Get(m,"size"));row.SubItems.Add(category);row.Tag=m;models.Items.Add(row);
  }
  description.SetBounds(22,452,836,88);description.Multiline=true;description.ReadOnly=true;description.ScrollBars=ScrollBars.Vertical;description.BackColor=BackColor;description.ForeColor=ForeColor;description.Text="Нажмите на модель, чтобы прочитать описание и выбрать диск.";Controls.Add(description);
  Controls.Add(new Label{Text="Папка выбранной модели (пусто — папка приложения/data/models):",Left=22,Top=550,Width=836,Height=24});
  modelFolder.SetBounds(22,578,715,28);modelFolder.Enabled=false;Controls.Add(modelFolder);modelBrowse.Text="Папка…";modelBrowse.SetBounds(748,576,110,30);modelBrowse.Enabled=false;modelBrowse.Click+=(s,e)=>PickFolder(modelFolder);Controls.Add(modelBrowse);
  models.SelectedIndexChanged+=(s,e)=>{if(models.SelectedItems.Count==0)return;var m=(Dictionary<string,object>)models.SelectedItems[0].Tag;description.Text=Get(m,"description")+"\r\n"+Get(m,"source");modelFolder.Enabled=modelBrowse.Enabled=!running;modelFolder.Text=Get(m,"directory");};
  modelFolder.TextChanged+=(s,e)=>{if(models.SelectedItems.Count>0)((Dictionary<string,object>)models.SelectedItems[0].Tag)["directory"]=modelFolder.Text;};
  install.Text="Установить и запустить";install.SetBounds(22,620,836,36);install.Click+=Install;Controls.Add(install);
  log.Multiline=true;log.ReadOnly=true;log.ScrollBars=ScrollBars.Vertical;log.SetBounds(22,668,836,80);log.BackColor=BackColor;log.ForeColor=ForeColor;Controls.Add(log);
  FormClosing+=(s,e)=>{if(running){e.Cancel=true;MessageBox.Show("Дождитесь установки. Текущая операция показана в журнале.");}};
 }
 static string Get(Dictionary<string,object> d,string key){return d.ContainsKey(key)?Convert.ToString(d[key]):"";}
 void PickFolder(TextBox field){using(var dialog=new FolderBrowserDialog()){if(Directory.Exists(field.Text))dialog.SelectedPath=field.Text;if(dialog.ShowDialog()==DialogResult.OK)field.Text=dialog.SelectedPath;}}
 public static void Extract(string destination){destination=Path.GetFullPath(destination);Directory.CreateDirectory(destination);using(var input=Assembly.GetExecutingAssembly().GetManifestResourceStream("payload.zip"))using(var archive=new ZipArchive(input,ZipArchiveMode.Read)){foreach(var entry in archive.Entries){string path=Path.GetFullPath(Path.Combine(destination,entry.FullName));if(!path.StartsWith(destination.TrimEnd(Path.DirectorySeparatorChar)+Path.DirectorySeparatorChar,StringComparison.OrdinalIgnoreCase))throw new Exception("Invalid archive path");Directory.CreateDirectory(Path.GetDirectoryName(path));using(var src=entry.Open())using(var dst=File.Create(path))src.CopyTo(dst);}File.WriteAllLines(Path.Combine(destination,"install-manifest.txt"),archive.Entries.Select(x=>x.FullName));}}
 async System.Threading.Tasks.Task Run(string binary,string args,string root){
  var start=new ProcessStartInfo(binary,args){WorkingDirectory=root,UseShellExecute=false,CreateNoWindow=true,RedirectStandardOutput=true,RedirectStandardError=true};start.EnvironmentVariables["PYTHONUTF8"]="1";start.EnvironmentVariables["PYTHONIOENCODING"]="utf-8";start.EnvironmentVariables["TEMP"]=Path.Combine(root,"data","temp");start.EnvironmentVariables["TMP"]=start.EnvironmentVariables["TEMP"];
  using(var process=new Process{StartInfo=start}){process.OutputDataReceived+=(s,a)=>{if(a.Data!=null)BeginInvoke((Action)(()=>log.AppendText(a.Data+"\r\n")));};process.ErrorDataReceived+=(s,a)=>{if(a.Data!=null)BeginInvoke((Action)(()=>log.AppendText(a.Data+"\r\n")));};process.Start();process.BeginOutputReadLine();process.BeginErrorReadLine();await System.Threading.Tasks.Task.Run(()=>process.WaitForExit());if(process.ExitCode!=0)throw new Exception("Ошибка "+process.ExitCode+". Подробности в журнале.");}
 }
 async void Install(object sender,EventArgs e){running=true;install.Enabled=folder.Enabled=browse.Enabled=models.Enabled=modelFolder.Enabled=modelBrowse.Enabled=false;try{
  string root=Path.GetFullPath(folder.Text);if(!Directory.Exists(Path.GetPathRoot(root)))throw new Exception("Диск не найден.");
  var selected=models.CheckedItems.Cast<ListViewItem>().Select(x=>(Dictionary<string,object>)x.Tag).Select(m=>new {model=Get(m,"id"),directory=Get(m,"directory")}).ToArray();foreach(var m in selected)if(m.directory!=""&&!Path.IsPathRooted(m.directory))throw new Exception("Укажите абсолютную папку модели: "+m.model);
  log.Text="Распаковка компонентов без сети…\r\n";await System.Threading.Tasks.Task.Run(()=>Extract(root));Directory.CreateDirectory(Path.Combine(root,"data","temp"));
  await Run("powershell.exe","-NoProfile -ExecutionPolicy Bypass -File \""+Path.Combine(root,"scripts","provision-windows.ps1")+"\" -Offline -TaskRoot \""+root+"\"",root);
  await Run("powershell.exe","-NoProfile -ExecutionPolicy Bypass -File \""+Path.Combine(root,"scripts","ensure-webview.ps1")+"\" -TaskRoot \""+root+"\"",root);
  if(selected.Length>0){File.WriteAllText(Path.Combine(root,"data","installer-models.json"),json.Serialize(selected),new System.Text.UTF8Encoding(false));log.AppendText("Загрузка выбранных моделей. Требуется интернет.\r\n");await Run(Path.Combine(root,"runtime","python","python.exe"),"\""+Path.Combine(root,"scripts","install_models.py")+"\" \""+root+"\"",root);}
  Process.Start(new ProcessStartInfo(Path.Combine(root,"LocalGemmaStudio.exe")){WorkingDirectory=root});running=false;Close();
 }catch(Exception error){log.AppendText("\r\n"+error.Message+"\r\nПосле распаковки приложение можно запустить отдельно; модели доступны в настройках.");running=false;install.Enabled=folder.Enabled=browse.Enabled=models.Enabled=true;}}
 [STAThread]static void Main(string[] args){if(args.Length==2&&args[0]=="--extract-only"){Extract(args[1]);return;}Application.EnableVisualStyles();Application.Run(new Setup());}
}

using System;
using System.IO;
using System.IO.Compression;
using System.Security.Cryptography;
using System.Text;
using System.Threading;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
using System.Reflection;
using System.Diagnostics;
using System.Drawing;
using System.Linq;
using System.Collections.Generic;
using System.Web.Script.Serialization;
using System.Windows.Forms;
// Publishing a release is a same-volume directory transaction. The journal is
// committed before launching or touching shortcuts/registration. A previous
// release and failed stages are retained; user data is moved, never copied/deleted.
public sealed class InstallTransaction {
 public sealed class Journal { public int Version=1; public string Root,Stage,Backup,Phase; public bool HadRoot; public List<string> Carry=new List<string>(); }
 public sealed class PackageFile { public string Path,Sha256; public long Length; }
 static readonly JavaScriptSerializer Json=new JavaScriptSerializer{MaxJsonLength=int.MaxValue};
 static string Full(string path){return Path.GetFullPath(path).TrimEnd(Path.DirectorySeparatorChar,Path.AltDirectorySeparatorChar);}
 [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)]static extern SafeFileHandle CreateFile(string name,uint access,uint share,IntPtr security,uint creation,uint flags,IntPtr template);
 [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)]static extern uint GetFinalPathNameByHandle(SafeFileHandle handle,StringBuilder path,uint length,uint flags);
 static string Physical(string path){using(var handle=CreateFile(path,0,7,IntPtr.Zero,3,0x02000000,IntPtr.Zero)){if(handle.IsInvalid)throw new IOException("Не удалось определить физическую папку: "+path);var text=new StringBuilder(32768);uint length=GetFinalPathNameByHandle(handle,text,(uint)text.Capacity,0);if(length==0||length>=text.Capacity)throw new IOException("Не удалось определить полный путь: "+path);string value=text.ToString();return value.StartsWith(@"\\?\UNC\",StringComparison.OrdinalIgnoreCase)?@"\\"+value.Substring(8):value.StartsWith(@"\\?\",StringComparison.Ordinal)?value.Substring(4):value;}}
 static string Root(string path){string root=Full(path),parent=Path.GetDirectoryName(root);if(String.IsNullOrEmpty(parent)||String.Equals(root,Path.GetPathRoot(root),StringComparison.OrdinalIgnoreCase))throw new IOException("Выберите отдельную папку приложения, не корень диска.");Directory.CreateDirectory(parent);if(File.Exists(root))throw new IOException("Путь установки занят файлом.");if(Directory.Exists(root)&&Reparse(root))throw new IOException("Папка установки не должна быть ссылкой или junction.");return Full(Directory.Exists(root)?Physical(root):Path.Combine(Physical(parent),Path.GetFileName(root)));}
 public static string PublicationName(string root){string value=new String(Full(root).Replace('/','\\').Select(c=>c>='a'&&c<='z'?(char)(c-32):c).ToArray());using(var sha=SHA256.Create())return @"Global\LocalGemmaStudio.Install."+BitConverter.ToString(sha.ComputeHash(Encoding.UTF8.GetBytes(value))).Replace("-","").ToLowerInvariant();}
 public sealed class PublicationGuard:IDisposable{readonly Mutex mutex;public PublicationGuard(string root){mutex=new Mutex(false,PublicationName(root));try{mutex.WaitOne();}catch(AbandonedMutexException){}}public void Dispose(){mutex.ReleaseMutex();mutex.Dispose();}}

 static bool Reparse(string path){return (File.GetAttributes(path)&FileAttributes.ReparsePoint)!=0;}
 static string Prefix(string root,string suffix){return Path.Combine(Path.GetDirectoryName(root),"."+Path.GetFileName(root)+suffix);}
 static string JournalPath(string root){return Prefix(root,".update.json");}
 static string Relative(string path){
  string value=path.Replace('\\','/');var parts=value.Split('/');
  if(String.IsNullOrEmpty(value)||Path.IsPathRooted(value)||parts.Any(p=>String.IsNullOrEmpty(p)||p=="."||p==".."||p.EndsWith(".")||p.EndsWith(" ")||p.IndexOfAny(new[]{':','\0','<','>','"','|','?','*'})>=0))throw new IOException("Недопустимый путь пакета: "+path);
  return String.Join("/",parts);
 }
 static string Under(string root,string relative){relative=Relative(relative);string path=Path.GetFullPath(Path.Combine(root,relative.Replace('/',Path.DirectorySeparatorChar)));if(!path.StartsWith(root+Path.DirectorySeparatorChar,StringComparison.OrdinalIgnoreCase))throw new IOException("Путь выходит за папку установки.");return path;}
 static void NoLinkedParents(string root,string relative){string path=root;var parts=Relative(relative).Split('/');for(int i=0;i<parts.Length;i++){path=Path.Combine(path,parts[i]);if((File.Exists(path)||Directory.Exists(path))&&Reparse(path))throw new IOException("Компонент заменяемой версии является ссылкой: "+path);}}
 static void Write(string path,string text){if((File.Exists(path)||Directory.Exists(path))&&Reparse(path))throw new IOException("Файл установки является ссылкой: "+path);string temp=path+"."+Guid.NewGuid().ToString("N")+".tmp";using(var file=new FileStream(temp,FileMode.CreateNew,FileAccess.Write,FileShare.None)){var bytes=new UTF8Encoding(false).GetBytes(text);file.Write(bytes,0,bytes.Length);file.Flush(true);}if(File.Exists(path))File.Replace(temp,path,null);else File.Move(temp,path);}
 static void Save(Journal state){Write(JournalPath(state.Root),Json.Serialize(state));}
 static void Point(Action<string> fault,string point){if(fault!=null)fault(point);}
 static FileStream Lock(string root){string path=Prefix(root,".update.lock");if(File.Exists(path)&&Reparse(path))throw new IOException("Блокировка установщика является ссылкой.");return new FileStream(path,FileMode.OpenOrCreate,FileAccess.ReadWrite,FileShare.None);}
 sealed class Lease:IDisposable {
  readonly FileStream file;
  public Lease(string home){Directory.CreateDirectory(Path.Combine(home,"data"));file=new FileStream(Path.Combine(home,"data","instance.lock"),FileMode.OpenOrCreate,FileAccess.ReadWrite,FileShare.ReadWrite|FileShare.Delete);try{file.Lock(0,long.MaxValue);}catch{file.Dispose();throw new IOException("Приложение или его задача работает. Закройте приложение перед обновлением; оно не остановлено установщиком.");}}
  public void Dispose(){file.Unlock(0,long.MaxValue);file.Dispose();}
 }
 static void Validate(Journal state,string root){
  if(state==null||state.Version!=1||!String.Equals(state.Root,root,StringComparison.OrdinalIgnoreCase)||state.Carry==null)throw new IOException("Журнал обновления повреждён; файлы сохранены для восстановления.");
  foreach(var item in new[]{new[]{state.Stage,".stage-"},new[]{state.Backup,".backup-"}}){string prefix=Prefix(root,item[1]);Guid id;if(item[0]==null||!item[0].StartsWith(prefix,StringComparison.OrdinalIgnoreCase)||!Guid.TryParseExact(item[0].Substring(prefix.Length),"N",out id)||Full(item[0])!=item[0]||(Directory.Exists(item[0])&&Reparse(item[0])))throw new IOException("Небезопасный путь в журнале обновления.");}
  if(!new[]{"staging","prepared","old_moved","carried","published","committed"}.Contains(state.Phase))throw new IOException("Неизвестная фаза обновления.");
  var distinct=new HashSet<string>(StringComparer.OrdinalIgnoreCase);foreach(string relative in state.Carry)if(!distinct.Add(Relative(relative)))throw new IOException("Повтор пути в журнале обновления.");
 }
 static void Move(string sourceRoot,string targetRoot,string relative){
  string source=Under(sourceRoot,relative),target=Under(targetRoot,relative);if(File.Exists(target)||Directory.Exists(target))throw new IOException("Конфликт восстановления: "+target);
  // The entry itself may be a preserved junction; never enumerate its target.
  string parent=Path.GetDirectoryName(relative.Replace('/',Path.DirectorySeparatorChar));if(!String.IsNullOrEmpty(parent)){NoLinkedParents(sourceRoot,parent);NoLinkedParents(targetRoot,parent);}
  Directory.CreateDirectory(Path.GetDirectoryName(target));if(Directory.Exists(source))Directory.Move(source,target);else if(File.Exists(source))File.Move(source,target);else throw new IOException("Переносимый путь пропал: "+source);
 }
 static bool Exists(string root,string relative){string path=Under(root,relative);return File.Exists(path)||Directory.Exists(path);}
 static void RecoverCore(string root,Action<string> report,Action<string> fault=null){if(!File.Exists(JournalPath(root)))return;using(var publication=new PublicationGuard(root)){RecoverLocked(root,report,fault);}}
 static void RecoverLocked(string root,Action<string> report,Action<string> fault){
  string journal=JournalPath(root);if(!File.Exists(journal))return;if(Reparse(journal))throw new IOException("Журнал обновления является ссылкой.");Journal state=Json.Deserialize<Journal>(File.ReadAllText(journal,Encoding.UTF8));Validate(state,root);
  // Directory publication itself is the commit point. Never roll back a full
  // published version after its first launch, even if the journal write crashed.
  string marker=Path.Combine(root,"install-publication.txt");
  if(Directory.Exists(root)&&File.Exists(marker))NoLinkedParents(root,"install-publication.txt");
  if(Directory.Exists(root)&&!Directory.Exists(state.Stage)&&File.Exists(marker)&&File.ReadAllText(marker,Encoding.UTF8)==state.Stage){state.Phase="committed";Save(state);}
  if(state.Phase=="committed"){if(!Directory.Exists(root))throw new IOException("Зафиксированная версия отсутствует; резервная версия сохранена: "+state.Backup);File.Delete(journal);if(report!=null)report("Предыдущее обновление уже зафиксировано; откат не выполнялся.");return;}
  if(!Directory.Exists(state.Backup)&&state.HadRoot){if(!Directory.Exists(root))throw new IOException("Не найдена прежняя папка приложения; журнал сохранён.");File.Delete(journal);if(report!=null)report("Прежняя версия не переключалась; staging сохранён: "+state.Stage);return;}
  string home=Directory.Exists(Path.Combine(root,"data"))?root:Directory.Exists(Path.Combine(state.Stage,"data"))?state.Stage:state.HadRoot?state.Backup:state.Stage;
  using(var lease=new Lease(home)){}
   if(Directory.Exists(root)){
    if(Directory.Exists(state.Stage))throw new IOException("Есть одновременно новая папка и staging; автоматический откат не затирает их.");
    Directory.Move(root,state.Stage);
   }
   if(state.HadRoot){
    int n=0;foreach(string relative in state.Carry.AsEnumerable().Reverse()){
     bool old=Exists(state.Backup,relative),moved=Exists(state.Stage,relative);
     if(old&&moved)throw new IOException("Конфликт пользовательских файлов при восстановлении: "+relative);
     if(moved)Move(state.Stage,state.Backup,relative);else if(!old)throw new IOException("Пользовательский путь отсутствует в обеих версиях: "+relative);Point(fault,"recovery_after_carry_"+(n++));
    }
    Directory.Move(state.Backup,root);Point(fault,"recovery_after_restore");
   }
   File.Delete(journal);
  if(report!=null)report("Прежнее расположение восстановлено. Неопубликованные файлы сохранены: "+state.Stage);
 }
 public static void Recover(string destination,Action<string> report,Action<string> fault=null){string root=Root(destination);using(var gate=Lock(root)){RecoverCore(root,report,fault);}}
 static List<PackageFile> Extract(Stream input,string stage){
  var files=new List<PackageFile>();var seen=new HashSet<string>(StringComparer.OrdinalIgnoreCase);
  using(var archive=new ZipArchive(input,ZipArchiveMode.Read,true)){
   long bytes=0;foreach(var entry in archive.Entries){checked{bytes+=entry.Length;}}
   var drive=new DriveInfo(Path.GetPathRoot(stage));if(drive.IsReady&&drive.AvailableFreeSpace<bytes)throw new IOException("Недостаточно места для полной подготовленной версии. Старая версия не изменена.");
   foreach(var entry in archive.Entries){bool directory=entry.FullName.EndsWith("/")||entry.FullName.EndsWith("\\");string relative=Relative(directory?entry.FullName.TrimEnd('/','\\'):entry.FullName);if(!seen.Add(relative))throw new IOException("Повтор пути в архиве: "+relative);if(relative.Split('/')[0].Equals("data",StringComparison.OrdinalIgnoreCase)||relative.Equals("install-manifest.json",StringComparison.OrdinalIgnoreCase)||relative.Equals("install-manifest.txt",StringComparison.OrdinalIgnoreCase)||relative.Equals("install-publication.txt",StringComparison.OrdinalIgnoreCase))throw new IOException("Пакет не может содержать данные или готовый install manifest.");string path=Under(stage,relative);if(directory){Directory.CreateDirectory(path);continue;}Directory.CreateDirectory(Path.GetDirectoryName(path));using(var src=entry.Open())using(var dst=new FileStream(path,FileMode.CreateNew,FileAccess.Write,FileShare.None)){src.CopyTo(dst);dst.Flush(true);}if(new FileInfo(path).Length!=entry.Length)throw new IOException("Размер распакованного файла не совпадает: "+relative);files.Add(Receipt(stage,relative));}
  }
  return files;
 }
 static PackageFile Receipt(string root,string relative){using(var file=File.OpenRead(Under(root,relative)))using(var sha=SHA256.Create()){return new PackageFile{Path=relative,Length=file.Length,Sha256=BitConverter.ToString(sha.ComputeHash(file)).Replace("-","").ToLowerInvariant()};}}
 static void Verify(string root,IEnumerable<PackageFile> files){foreach(var expected in files){NoLinkedParents(root,expected.Path);var actual=Receipt(root,expected.Path);if(actual.Length!=expected.Length||actual.Sha256!=expected.Sha256)throw new IOException("Файл staging изменился после проверки: "+expected.Path);}}
 static List<PackageFile> Inventory(string stage){var files=new List<PackageFile>();var directories=new Stack<string>();directories.Push("");while(directories.Count>0){string relative=directories.Pop();foreach(string path in Directory.EnumerateFileSystemEntries(relative==""?stage:Under(stage,relative))){string child=relative==""?Path.GetFileName(path):relative+"/"+Path.GetFileName(path);if(child.Equals("data",StringComparison.OrdinalIgnoreCase))continue;if(Reparse(path))throw new IOException("Проверенная версия содержит ссылку: "+child);if(Directory.Exists(path))directories.Push(child);else files.Add(Receipt(stage,child));}}return files;}
 static HashSet<string> OldManaged(string root){var result=new HashSet<string>(StringComparer.OrdinalIgnoreCase);string path=Path.Combine(root,"install-manifest.txt");if(File.Exists(path)){NoLinkedParents(root,"install-manifest.txt");foreach(string line in File.ReadAllLines(path))if(!String.IsNullOrWhiteSpace(line))result.Add(Relative(line));}result.Add("install-manifest.txt");result.Add("install-manifest.json");return result;}
 static void PlanCarry(string old,string stage,string relative,HashSet<string> managed,List<string> carry){
  foreach(string path in Directory.EnumerateFileSystemEntries(String.IsNullOrEmpty(relative)?old:Under(old,relative))){string child=String.IsNullOrEmpty(relative)?Path.GetFileName(path):relative+"/"+Path.GetFileName(path);child=Relative(child);string next=Under(stage,child);bool dir=Directory.Exists(path),targetDir=Directory.Exists(next),targetFile=File.Exists(next);
   if(dir){if(targetFile)throw new IOException("Файл новой версии конфликтует с папкой: "+child);if(Reparse(path)){if(targetDir)throw new IOException("Junction конфликтует с компонентом новой версии: "+child);carry.Add(child);}else if(targetDir||managed.Any(p=>p.StartsWith(child+"/",StringComparison.OrdinalIgnoreCase)))PlanCarry(old,stage,child,managed,carry);else carry.Add(child);}
   else{if(targetDir)throw new IOException("Папка новой версии конфликтует с файлом: "+child);if(!targetFile&&!managed.Contains(child))carry.Add(child);}
  }
 }
 static void Preflight(string root,IEnumerable<PackageFile> files){foreach(var file in files){NoLinkedParents(root,file.Path);string path=Under(root,file.Path);if(Directory.Exists(path))throw new IOException("Файл пакета занят папкой: "+file.Path);if(File.Exists(path))using(var probe=new FileStream(path,FileMode.Open,FileAccess.ReadWrite,FileShare.None)){} }}
 public static string Apply(string destination,Stream payload,Action<string> verify,Action<string> fault,Action<string> report){
  string root=Root(destination);using(var gate=Lock(root)){
   RecoverCore(root,report);var state=new Journal{Root=root,Stage=Prefix(root,".stage-"+Guid.NewGuid().ToString("N")),Backup=Prefix(root,".backup-"+Guid.NewGuid().ToString("N")),HadRoot=Directory.Exists(root),Phase="staging"};Save(state);
   try{
    Directory.CreateDirectory(state.Stage);var original=Extract(payload,state.Stage);Verify(state.Stage,original);Point(fault,"after_extract");
    if(verify!=null)verify(state.Stage);Verify(state.Stage,original.Where(file=>!file.Path.Equals("runtime/python/python313._pth",StringComparison.OrdinalIgnoreCase)));Point(fault,"after_verify");
    // Offline verification writes only a temporary data tree, never live data.
    // Preserve that private tree separately so it cannot replace existing data.
    string preparedData=Path.Combine(state.Stage,"data");if(Directory.Exists(preparedData))Directory.Move(preparedData,Prefix(root,".verify-data-"+Guid.NewGuid().ToString("N")));
    Write(Path.Combine(state.Stage,"install-publication.txt"),state.Stage);
    var files=Inventory(state.Stage);
    Write(Path.Combine(state.Stage,"install-manifest.txt"),String.Join(Environment.NewLine,files.Select(f=>f.Path)));Write(Path.Combine(state.Stage,"install-manifest.json"),Json.Serialize(new{version=1,files=files,previous_backup=state.HadRoot?state.Backup:null}));
    if(!state.HadRoot)Directory.CreateDirectory(Path.Combine(state.Stage,"data"));
    using(var publication=new PublicationGuard(root)){
    using(var lease=new Lease(state.HadRoot?root:state.Stage)){
     if(state.HadRoot){Preflight(root,files);PlanCarry(root,state.Stage,"",OldManaged(root),state.Carry);}
    }
     state.Phase="prepared";Save(state);Point(fault,"before_old_move");
     if(state.HadRoot)Directory.Move(root,state.Backup);Point(fault,"after_old_move");state.Phase="old_moved";Save(state);
     int n=0;foreach(string relative in state.Carry){Move(state.Backup,state.Stage,relative);Point(fault,"after_carry_"+(n++));}
     state.Phase="carried";Save(state);Point(fault,"after_carry");Verify(state.Stage,files);
     Directory.Move(state.Stage,root);Point(fault,"after_publish");state.Phase="published";Save(state);
     state.Phase="committed";Save(state);Point(fault,"after_commit");
    }
    File.Delete(JournalPath(root));if(report!=null)report("Версия установлена. Прежние файлы сохранены: "+state.Backup);return root;
   }catch(Exception error){try{RecoverCore(root,report);}catch(Exception recovery){throw new IOException(error.Message+"\r\nАвтоматическое восстановление не завершилось: "+recovery.Message+"\r\nЖурнал: "+JournalPath(root),error);}throw;}
  }
 }
}

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
 public static void Extract(string destination){using(var input=Assembly.GetExecutingAssembly().GetManifestResourceStream("payload.zip"))InstallTransaction.Apply(destination,input,null,null,null);}
 async System.Threading.Tasks.Task Run(string binary,string args,string root){
  var start=new ProcessStartInfo(binary,args){WorkingDirectory=root,UseShellExecute=false,CreateNoWindow=true,RedirectStandardOutput=true,RedirectStandardError=true};start.EnvironmentVariables["PYTHONUTF8"]="1";start.EnvironmentVariables["PYTHONIOENCODING"]="utf-8";start.EnvironmentVariables["TEMP"]=Path.Combine(root,"data","temp");start.EnvironmentVariables["TMP"]=start.EnvironmentVariables["TEMP"];
  using(var process=new Process{StartInfo=start}){process.OutputDataReceived+=(s,a)=>{if(a.Data!=null)BeginInvoke((Action)(()=>log.AppendText(a.Data+"\r\n")));};process.ErrorDataReceived+=(s,a)=>{if(a.Data!=null)BeginInvoke((Action)(()=>log.AppendText(a.Data+"\r\n")));};process.Start();process.BeginOutputReadLine();process.BeginErrorReadLine();await System.Threading.Tasks.Task.Run(()=>process.WaitForExit());if(process.ExitCode!=0)throw new Exception("Ошибка "+process.ExitCode+". Подробности в журнале.");}
 }
 async void Install(object sender,EventArgs e){running=true;install.Enabled=folder.Enabled=browse.Enabled=models.Enabled=modelFolder.Enabled=modelBrowse.Enabled=false;try{
  string root=Path.GetFullPath(folder.Text);if(!Directory.Exists(Path.GetPathRoot(root)))throw new Exception("Диск не найден.");
  var selected=models.CheckedItems.Cast<ListViewItem>().Select(x=>(Dictionary<string,object>)x.Tag).Select(m=>new {model=Get(m,"id"),directory=Get(m,"directory")}).ToArray();foreach(var m in selected)if(m.directory!=""&&!Path.IsPathRooted(m.directory))throw new Exception("Укажите абсолютную папку модели: "+m.model);
  log.Text="Подготовка и проверка новой версии…\r\n";
  await System.Threading.Tasks.Task.Run(()=>{using(var input=Assembly.GetExecutingAssembly().GetManifestResourceStream("payload.zip"))InstallTransaction.Apply(root,input,stage=>{
   Run("powershell.exe","-NoProfile -ExecutionPolicy Bypass -File \""+Path.Combine(stage,"scripts","provision-windows.ps1")+"\" -Offline -VerifyOnly -TaskRoot \""+stage+"\"",stage).GetAwaiter().GetResult();
  },null,message=>BeginInvoke((Action)(()=>log.AppendText(message+"\r\n"))));});
  Directory.CreateDirectory(Path.Combine(root,"data","temp"));
  await Run("powershell.exe","-NoProfile -ExecutionPolicy Bypass -File \""+Path.Combine(root,"scripts","provision-windows.ps1")+"\" -Offline -TaskRoot \""+root+"\"",root);
  await Run("powershell.exe","-NoProfile -ExecutionPolicy Bypass -File \""+Path.Combine(root,"scripts","ensure-webview.ps1")+"\" -TaskRoot \""+root+"\"",root);
  if(selected.Length>0){File.WriteAllText(Path.Combine(root,"data","installer-models.json"),json.Serialize(selected),new System.Text.UTF8Encoding(false));log.AppendText("Загрузка выбранных моделей. Требуется интернет.\r\n");await Run(Path.Combine(root,"runtime","python","python.exe"),"\""+Path.Combine(root,"scripts","install_models.py")+"\" \""+root+"\"",root);}
  Process.Start(new ProcessStartInfo(Path.Combine(root,"LocalGemmaStudio.exe")){WorkingDirectory=root});running=false;Close();
 }catch(Exception error){log.AppendText("\r\n"+error.Message+"\r\nПодробности восстановления и сохранённые папки указаны выше. Загрузка моделей и запуск выполняются после фиксации версии; их ошибка не откатывает данные.");running=false;install.Enabled=folder.Enabled=browse.Enabled=models.Enabled=true;}}
 [STAThread]static void Main(string[] args){if(args.Length==2&&args[0]=="--extract-only"){Extract(args[1]);return;}Application.EnableVisualStyles();Application.Run(new Setup());}
}

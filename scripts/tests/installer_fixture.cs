using System;
using System.IO;
using System.Threading;
class InstallerFixture {
 public static int Main(string[] args){Console.OutputEncoding=new System.Text.UTF8Encoding(false);try{
  string operation=args[0],root=Path.GetFullPath(args[1]);
  if(operation=="name"){Console.WriteLine(InstallTransaction.PublicationName(root));return 0;}
  if(operation=="guard"){using(var guard=new InstallTransaction.PublicationGuard(root)){Console.WriteLine("LOCKED");Console.Out.Flush();Thread.Sleep(30000);}return 0;}
  if(operation=="recover"){InstallTransaction.Recover(root,Console.WriteLine,point=>{if(args.Length>3&&args[3]==point)Environment.Exit(72);});return 0;}
  if(operation=="lease"){Directory.CreateDirectory(Path.Combine(root,"data"));using(var file=new FileStream(Path.Combine(root,"data","instance.lock"),FileMode.OpenOrCreate,FileAccess.ReadWrite,FileShare.ReadWrite|FileShare.Delete)){file.Lock(0,long.MaxValue);Console.WriteLine("LOCKED");Console.Out.Flush();Thread.Sleep(30000);}return 0;}
  using(var payload=File.OpenRead(args[2]))InstallTransaction.Apply(root,payload,stage=>{
   if(args.Length>3&&args[3]=="verify_signal"){Console.WriteLine("STAGED");Console.Out.Flush();}
   if(args.Length>3&&args[3]=="verify_wait"){Console.WriteLine("VERIFY_WAIT");Console.Out.Flush();Thread.Sleep(30000);}
   if(args.Length>3&&args[3]=="verify_fail")throw new IOException("Injected component verification failure");
   if(args.Length>3&&args[3]=="verify_modify")File.AppendAllText(Path.Combine(stage,"runtime","worker.txt")," VERIFIED");
  },point=>{if(args.Length>3&&args[3]=="tamper"&&point=="after_carry"){var json=new System.Web.Script.Serialization.JavaScriptSerializer();var journal=json.Deserialize<System.Collections.Generic.Dictionary<string,object>>(File.ReadAllText(Path.Combine(Path.GetDirectoryName(root),"."+Path.GetFileName(root)+".update.json")));File.AppendAllText(Path.Combine((string)journal["Stage"],"runtime","worker.txt"),"TAMPERED");}if(args.Length>3&&args[3]==point){Console.WriteLine(point);Console.Out.Flush();Environment.Exit(71);}},Console.WriteLine);return 0;
 }catch(Exception error){Console.Error.WriteLine(error.ToString());return 3;}}
}

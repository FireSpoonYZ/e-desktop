using System;
using System.Collections.Generic;
using System.Drawing;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Web.Script.Serialization;
using System.Windows.Forms;
class SmokeFixture {
  [StructLayout(LayoutKind.Sequential)] public struct Rect { public int X,Y,Right,Bottom; }
  [DllImport("user32.dll")] static extern bool SetProcessDpiAwarenessContext(IntPtr value);
  [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out Rect rect);
  [DllImport("user32.dll")] static extern int GetWindowRgn(IntPtr h, IntPtr region);
  [DllImport("gdi32.dll")] static extern IntPtr CreateRectRgn(int x,int y,int r,int b);
  [DllImport("gdi32.dll")] static extern int GetRgnBox(IntPtr region,out Rect rect);
  [DllImport("gdi32.dll")] static extern bool DeleteObject(IntPtr region);
  [STAThread] static void Main(string[] args) {
    bool anchor = args.Contains("--anchor");
    SetProcessDpiAwarenessContext(new IntPtr(-4));
    Application.EnableVisualStyles();
    var context = new ApplicationContext();
    var forms = new List<Form>();
    var area = Screen.PrimaryScreen.WorkingArea;
    var colors = new [] { Color.LightSteelBlue, Color.Honeydew, Color.Wheat };
    for (int i=0; i<(anchor ? 1 : 3); i++) {
      var f = new Form();
      f.Text = anchor ? "e-desktop Smoke Launcher" : "e-desktop Smoke " + (char)(65+i);
      f.StartPosition = FormStartPosition.Manual;
      f.Bounds = new Rectangle(area.X+140+i*110,area.Y+120+i*80,640+i*40,440+i*30);
      f.BackColor = colors[i];
      f.Controls.Add(new Label { Dock=DockStyle.Fill,TextAlign=ContentAlignment.MiddleCenter,Font=new Font("Segoe UI",22),Text=f.Text+"\nDisposable native test window" });
      forms.Add(f); f.Show();
    }
    var timer = new Timer { Interval=250 };
    timer.Tick += delegate {
      var states = new List<object>();
      foreach (var f in forms.Where(f => !f.IsDisposed)) {
        Rect r, clip; GetWindowRect(f.Handle,out r);
        var region = CreateRectRgn(0,0,0,0); int kind = GetWindowRgn(f.Handle,region);
        GetRgnBox(region,out clip); DeleteObject(region);
        states.Add(new { title=f.Text,hwnd=f.Handle.ToInt64(),x=r.X,y=r.Y,width=r.Right-r.X,height=r.Bottom-r.Y,state=f.WindowState.ToString(),visible=f.Visible,focused=GetForegroundWindow()==f.Handle,regionKind=kind,clip=clip });
      }
      var json = new JavaScriptSerializer().Serialize(new {pid=System.Diagnostics.Process.GetCurrentProcess().Id,windows=states,captured=DateTime.Now.ToString("O")});
      File.WriteAllText(Path.Combine(Path.GetTempPath(),anchor ? "e-desktop-smoke-anchor.json" : "e-desktop-smoke-state.json"),json);
      if (states.Count==0) context.ExitThread();
    };
    timer.Start(); Application.Run(context); timer.Dispose();
  }
}

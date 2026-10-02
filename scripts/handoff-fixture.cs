using System;
using System.Diagnostics;
using System.Drawing;
using System.Drawing.Imaging;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.RegularExpressions;
using System.Web.Script.Serialization;
using System.Windows.Forms;

// Offline marker contract: 12x10 cells, 4 physical pixels/cell, inset 8.
// Border identifies TL/TR/BL/BR; inner 10x8 bits are LSB-first bytes:
// generation uint32 LE, width uint16 LE, height uint16 LE, ASCII role, XOR checksum (seed A7).
class HandoffFixture {
  public const int Cell = 4, Inset = 8, MarkerWidth = 48, MarkerHeight = 40;
  public static readonly Color Zero = Color.FromArgb(16,16,16), One = Color.FromArgb(240,240,240);
  public static readonly Color[] Borders = {
    Color.FromArgb(0,208,208), Color.FromArgb(208,0,208),
    Color.FromArgb(208,208,0), Color.FromArgb(240,112,0)
  };
  [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
  [StructLayout(LayoutKind.Sequential)] struct Point { public int X, Y; }
  [DllImport("user32.dll")] static extern bool SetProcessDpiAwarenessContext(IntPtr context);
  [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
  [DllImport("user32.dll")] static extern bool GetClientRect(IntPtr hwnd, out Rect rect);
  [DllImport("user32.dll")] static extern bool ClientToScreen(IntPtr hwnd, ref Point point);
  [DllImport("user32.dll")] static extern uint GetDpiForWindow(IntPtr hwnd);
  [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr hwnd, int attribute, out Rect rect, int size);

  class Options {
    public string Tag, Role, StateFile, Reference;
    public int Delay, Width = 702, Height = 491;
    public uint Generation = 1;
    public bool SelfTest;
    public static Options Parse(string[] args) {
      var o = new Options();
      for (int i=0; i<args.Length; i++) {
        string key = args[i];
        if (key == "--self-test") { o.SelfTest = true; continue; }
        if (++i == args.Length) throw new ArgumentException("Missing value for " + key);
        string value = args[i];
        switch (key) {
          case "--tag": o.Tag=value; break;
          case "--role": o.Role=value; break;
          case "--state-file": o.StateFile=Path.GetFullPath(value); break;
          case "--paint-delay-ms": o.Delay=int.Parse(value); break;
          case "--render-reference": o.Reference=Path.GetFullPath(value); break;
          case "--width": o.Width=int.Parse(value); break;
          case "--height": o.Height=int.Parse(value); break;
          case "--generation": o.Generation=uint.Parse(value); break;
          default: throw new ArgumentException("Unknown argument " + key);
        }
      }
      if (o.Delay<0 || o.Delay>2000) throw new ArgumentException("paint-delay-ms must be 0..2000");
      if (o.Generation==0) throw new ArgumentException("generation must be 1..4294967295");
      if (o.SelfTest) {
        if (args.Length!=1) throw new ArgumentException("--self-test must be used alone");
        return o;
      }
      if (o.Tag==null || !Regex.IsMatch(o.Tag, "^[A-Za-z0-9-]{1,64}$") ||
          o.Role==null || !Regex.IsMatch(o.Role, "^[A-E]$"))
        throw new ArgumentException("--tag (ASCII letters/digits/hyphen, 1..64) and --role A..E required");
      if (o.Width<128 || o.Height<112 || o.Width>4096 || o.Height>4096)
        throw new ArgumentException("Size must be 128..4096 by 112..4096");
      if (o.Reference==null && o.StateFile==null) throw new ArgumentException("--state-file required");
      if (o.Reference==null && o.Generation!=1) throw new ArgumentException("--generation is offline-only");
      return o;
    }
  }

  public static byte[] Payload(uint generation, int width, int height, char role) {
    byte[] bytes = new byte[10];
    for (int i=0; i<4; i++) bytes[i]=(byte)(generation>>(i*8));
    bytes[4]=(byte)width; bytes[5]=(byte)(width>>8);
    bytes[6]=(byte)height; bytes[7]=(byte)(height>>8);
    bytes[8]=(byte)role; bytes[9]=0xA7;
    for (int i=0; i<9; i++) bytes[9]^=bytes[i];
    return bytes;
  }
  static Rectangle MarkerRect(int corner, int width, int height) {
    return new Rectangle((corner%2==0) ? Inset : width-Inset-MarkerWidth,
                         (corner<2) ? Inset : height-Inset-MarkerHeight, MarkerWidth, MarkerHeight);
  }
  public static Bitmap Render(string tag, char role, uint generation, int width, int height) {
    if (width<128 || height<112 || width>4096 || height>4096) throw new ArgumentException("Unsupported bitmap size");
    var bitmap = new Bitmap(width,height,PixelFormat.Format32bppArgb);
    try {
      using (var g = Graphics.FromImage(bitmap)) {
        g.Clear(Color.FromArgb(42,54,70));
        // A deterministic interior as well as corner markers; reference comparison checks every client pixel.
        using (var stripe = new SolidBrush(Color.FromArgb(60+(int)(generation%80),70+(int)(generation%60),95))) {
          for (int y=0; y<height; y+=32)
            for (int x=0; x<width; x+=32)
              if (((x/32+y/32)&1)==0) g.FillRectangle(stripe,x,y,32,32);
        }
        using (var font = new Font("Segoe UI",14,FontStyle.Bold,GraphicsUnit.Pixel))
        using (var brush = new SolidBrush(Color.White)) {
          g.DrawString("e-desktop Handoff " + tag + " " + role + "\nDesired/Painted G" + generation + "/G" + generation + " (published)" +
                       "\nPainted client " + width + "x" + height, font, brush, 64,48);
        }
        byte[] payload = Payload(generation,width,height,role);
        for (int corner=0; corner<4; corner++) {
          Rectangle r = MarkerRect(corner,width,height);
          using (var border = new SolidBrush(Borders[corner])) g.FillRectangle(border,r);
          using (var zero = new SolidBrush(Zero))
          using (var one = new SolidBrush(One)) {
            for (int bit=0; bit<80; bit++)
              g.FillRectangle((payload[bit/8] & (1<<(bit%8)))==0 ? zero : one,
                r.X+(1+bit%10)*Cell, r.Y+(1+bit/10)*Cell, Cell,Cell);
          }
        }
      }
      return bitmap;
    } catch { bitmap.Dispose(); throw; }
  }

  static void DrawClient(Graphics graphics, Bitmap bitmap) {
    graphics.Clear(Color.FromArgb(26,26,26));
    // Never stretch old pixels while pending. Newly exposed space is explicitly empty.
    if (bitmap!=null) graphics.DrawImageUnscaled(bitmap,0,0);
  }

  class FixtureForm : Form {
    readonly Options options;
    readonly Timer timer = new Timer { Interval=10 };
    Bitmap painted;
    uint desired = 1, paintedGeneration;
    Size desiredSize;
    long due, nextHeartbeat, stateSequence, publishedAt;
    bool started;
    public FixtureForm(Options options) {
      this.options=options;
      Text="e-desktop Handoff " + options.Tag + " " + options.Role;
      AutoScaleMode=AutoScaleMode.None;
      StartPosition=FormStartPosition.Manual;
      Size=new Size(options.Width,options.Height);
      SetStyle(ControlStyles.AllPaintingInWmPaint | ControlStyles.UserPaint | ControlStyles.OptimizedDoubleBuffer,true);
      timer.Tick += delegate {
        long now=Stopwatch.GetTimestamp();
        if (due!=0 && now>=due) Publish();
        if (now>=nextHeartbeat) { WriteState(); nextHeartbeat=now+Stopwatch.Frequency/10; }
      };
    }
    protected override void OnShown(EventArgs e) {
      base.OnShown(e);
      MinimumSize=SizeFromClientSize(new Size(128,112));
      // GetWindowRect includes invisible resize borders. Tune once to the requested visible physical size.
      Rect outer, visible;
      if (!GetWindowRect(Handle,out outer) ||
          DwmGetWindowAttribute(Handle,9,out visible,Marshal.SizeOf(typeof(Rect)))!=0)
        throw new InvalidOperationException("Cannot measure initial physical visible frame");
      Width += options.Width-(visible.Right-visible.Left);
      Height += options.Height-(visible.Bottom-visible.Top);
      started=true;
      desiredSize=ClientSize;
      Publish();
      timer.Start();
    }
    protected override void OnResize(EventArgs e) {
      base.OnResize(e);
      if (!started || WindowState==FormWindowState.Minimized || ClientSize.Width==0 || ClientSize.Height==0) return;
      if (ClientSize!=desiredSize) {
        if (desired==uint.MaxValue) throw new InvalidOperationException("Generation exhausted");
        desired++;
        desiredSize=ClientSize;
        due=Stopwatch.GetTimestamp()+options.Delay*Stopwatch.Frequency/1000;
        if (options.Delay==0) Publish();
        else { Invalidate(); WriteState(); }
      }
    }
    void Publish() {
      var next=Render(options.Tag,options.Role[0],desired,desiredSize.Width,desiredSize.Height);
      var previous=painted;
      painted=next; paintedGeneration=desired; due=0; publishedAt=Stopwatch.GetTimestamp();
      if (previous!=null) previous.Dispose();
      Invalidate();
      WriteState();
    }
    protected override void OnPaintBackground(PaintEventArgs e) { }
    protected override void OnPaint(PaintEventArgs e) {
      DrawClient(e.Graphics,painted);
    }
    protected override void WndProc(ref Message m) {
      base.WndProc(ref m);
      if (started && (m.Msg==0x0003 || m.Msg==0x02E0)) WriteState(); // move / DPI changed
    }
    static object JsonRect(Rect r) {
      return new { x=r.Left,y=r.Top,width=r.Right-r.Left,height=r.Bottom-r.Top };
    }
    void WriteState() {
      if (!started || IsDisposed) return;
      Rect outer, client, visible;
      Point origin=new Point();
      if (!GetWindowRect(Handle,out outer) || !GetClientRect(Handle,out client) || !ClientToScreen(Handle,ref origin))
        throw new InvalidOperationException("Physical geometry query failed");
      client.Right+=origin.X; client.Bottom+=origin.Y; client.Left=origin.X; client.Top=origin.Y;
      bool visibleValid=DwmGetWindowAttribute(Handle,9,out visible,Marshal.SizeOf(typeof(Rect)))==0;
      var data=new {
        schemaVersion=1, tag=options.Tag, role=options.Role, title=Text,
        pid=Process.GetCurrentProcess().Id, hwnd="0x"+Handle.ToInt64().ToString("X"),
        sequence=++stateSequence, qpcTicks=Stopwatch.GetTimestamp(), qpcFrequency=Stopwatch.Frequency,
        clock="QueryPerformanceCounter", clockUnit="ticks",
        outerRect=JsonRect(outer), clientRect=JsonRect(client), visibleRect=visibleValid ? JsonRect(visible) : null,
        dpi=GetDpiForWindow(Handle), windowState=WindowState.ToString(), visible=Visible,
        desiredGeneration=desired, paintedGeneration=paintedGeneration,
        desiredClientSize=new { width=desiredSize.Width,height=desiredSize.Height },
        paintedClientSize=painted==null ? null : new { width=painted.Width,height=painted.Height },
        paintDelayMs=options.Delay, pending=due!=0, paintDueQpcTicks=due, bitmapPublishedQpcTicks=publishedAt,
        initialRequestedVisibleSize=new { width=options.Width,height=options.Height },
        marker=new { version=1, cellPixels=Cell, insetPixels=Inset, columns=12, rows=10 }
      };
      string path=options.StateFile, temp=path+".tmp";
      Directory.CreateDirectory(Path.GetDirectoryName(path));
      File.WriteAllText(temp,new JavaScriptSerializer().Serialize(data),new UTF8Encoding(false));
      if (File.Exists(path)) File.Replace(temp,path,null);
      else File.Move(temp,path);
    }
    protected override void Dispose(bool disposing) {
      if (disposing) { timer.Dispose(); if (painted!=null) { painted.Dispose(); painted=null; } }
      base.Dispose(disposing);
    }
  }

  static void Assert(bool condition, string message) {
    if (!condition) throw new Exception("self-test: "+message);
  }
  static void SelfTest() {
    // No Form, HWND, DPI mutation, or Application.Run in this branch.
    var bytes=Payload(0x12345678,640,480,'C');
    Assert(bytes[0]==0x78 && bytes[3]==0x12 && bytes[4]==0x80 && bytes[5]==2,"little endian");
    byte checksum=0xA7; for (int i=0;i<9;i++) checksum^=bytes[i];
    Assert(checksum==bytes[9],"checksum");
    using (var bitmap=Render("offline",'C',0x12345678,640,480)) {
      for (int corner=0;corner<4;corner++) {
        Rectangle r=MarkerRect(corner,640,480);
        Assert(bitmap.GetPixel(r.X,r.Y).ToArgb()==Borders[corner].ToArgb(),"corner border");
        for (int bit=0;bit<80;bit++) {
          Color expected=(bytes[bit/8]&(1<<(bit%8)))==0 ? Zero : One;
          Assert(bitmap.GetPixel(r.X+(1+bit%10)*Cell+2,r.Y+(1+bit/10)*Cell+2).ToArgb()==expected.ToArgb(),"marker bit");
        }
      }
    }
    using (var old=Render("offline",'C',7,160,128))
    using (var larger=new Bitmap(320,256,PixelFormat.Format32bppArgb)) {
      using (var graphics=Graphics.FromImage(larger)) DrawClient(graphics,old);
      Assert(larger.GetPixel(106,90).ToArgb()==old.GetPixel(106,90).ToArgb(),"old lower-right marker stays unscaled");
      Assert(larger.GetPixel(300,220).ToArgb()==Color.FromArgb(26,26,26).ToArgb(),"new area stays empty while pending");
    }
    foreach (int delay in new [] {0,2000}) {
      var parsed=Options.Parse(new [] {"--tag","offline","--role","C","--state-file","offline.json","--paint-delay-ms",delay.ToString()});
      Assert(parsed.Delay==delay,"delay boundary");
    }
    bool rejected=false;
    try { Options.Parse(new [] {"--tag","bad tag","--role","C","--state-file","offline.json"}); }
    catch (ArgumentException) { rejected=true; }
    Assert(rejected,"invalid tag rejected");
    rejected=false;
    try { Options.Parse(new [] {"--tag","ok","--role","C","--state-file","offline.json","--paint-delay-ms","2001"}); }
    catch (ArgumentException) { rejected=true; }
    Assert(rejected,"delay range");
    Console.WriteLine("PASS: offline bitmap marker/encoding/argument checks; no window created");
  }
  [STAThread] static int Main(string[] args) {
    try {
      Options options=Options.Parse(args);
      if (options.SelfTest) { SelfTest(); return 0; }
      if (options.Reference!=null) {
        Directory.CreateDirectory(Path.GetDirectoryName(options.Reference));
        using (var bitmap=Render(options.Tag,options.Role[0],options.Generation,options.Width,options.Height))
          bitmap.Save(options.Reference,ImageFormat.Bmp);
        Console.WriteLine("Offline reference written; no window created");
        return 0;
      }
      if (!SetProcessDpiAwarenessContext(new IntPtr(-4))) throw new InvalidOperationException("PMv2 DPI awareness required");
      Application.EnableVisualStyles();
      using (var form=new FixtureForm(options)) Application.Run(form);
      return 0;
    } catch (Exception e) { Console.Error.WriteLine(e.Message); return 1; }
  }
}

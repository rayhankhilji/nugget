// Native WinForms test app for the nugget-a11y UI Automation integration tests
// (crates/nugget-a11y/tests/windows_uia.rs). Built on the runner with the .NET Framework C#
// compiler (C# 5, no newer syntax): see build.ps1.
//
// Usage: NuggetA11yFixture.exe <state-file>
//
// The window "Nugget A11y Fixture" holds a multi-line composer ("Hello world", with "world"
// selected and focused), a single-line Name box, an Apply button (sets the status label and the
// window title), a Notifications check box and a Fruits list. Every 50 ms the app writes its
// own view of every control to <state-file> as one JSON object (atomically, via a rename), so
// the test can check what really happened independently of UI Automation.

using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Windows.Forms;

static class Program
{
    [STAThread]
    static int Main(string[] args)
    {
        if (args.Length < 1)
        {
            Console.Error.WriteLine("usage: NuggetA11yFixture.exe <state-file>");
            return 2;
        }
        Application.EnableVisualStyles();
        Application.SetCompatibleTextRenderingDefault(false);
        Application.Run(new FixtureForm(args[0]));
        return 0;
    }
}

class FixtureForm : Form
{
    const string BaseTitle = "Nugget A11y Fixture";

    [DllImport("user32.dll")]
    static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")]
    static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")]
    static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);

    readonly string statePath;
    readonly TextBox composer;
    readonly TextBox nameBox;
    readonly Button apply;
    readonly CheckBox notifications;
    readonly ListBox fruits;
    readonly Label status;
    readonly Timer timer;
    string lastState = "";
    int presses;
    int activateTries;

    public FixtureForm(string statePath)
    {
        this.statePath = statePath;
        Text = BaseTitle;
        ClientSize = new System.Drawing.Size(520, 420);
        StartPosition = FormStartPosition.Manual;
        Location = new System.Drawing.Point(80, 80);

        Label composerLabel = new Label();
        composerLabel.Text = "Message composer";
        composerLabel.SetBounds(12, 12, 200, 20);

        composer = new TextBox();
        composer.Name = "composer";
        composer.AccessibleName = "Message composer";
        composer.Multiline = true;
        composer.ScrollBars = ScrollBars.Vertical;
        composer.Text = "Hello world";
        composer.SetBounds(12, 34, 490, 110);

        Label nameLabel = new Label();
        nameLabel.Text = "Name";
        nameLabel.SetBounds(12, 156, 60, 20);

        nameBox = new TextBox();
        nameBox.Name = "name";
        nameBox.AccessibleName = "Name";
        nameBox.SetBounds(80, 154, 220, 24);

        apply = new Button();
        apply.Name = "apply";
        apply.Text = "Apply";
        apply.SetBounds(12, 190, 100, 30);
        apply.Click += delegate
        {
            presses++;
            status.Text = "Status: applied " + presses;
            Text = BaseTitle + " - applied " + presses;
        };

        notifications = new CheckBox();
        notifications.Name = "notifications";
        notifications.Text = "Notifications";
        notifications.SetBounds(130, 194, 160, 24);

        fruits = new ListBox();
        fruits.Name = "fruits";
        fruits.AccessibleName = "Fruits";
        fruits.Items.AddRange(new object[] { "Apple", "Banana", "Cherry" });
        fruits.SetBounds(12, 232, 200, 80);

        status = new Label();
        status.Name = "status";
        status.Text = "Status: idle";
        status.SetBounds(12, 330, 300, 24);

        Controls.AddRange(new Control[] {
            composerLabel, composer, nameLabel, nameBox, apply, notifications, fruits, status
        });

        timer = new Timer();
        timer.Interval = 50;
        timer.Tick += delegate { Tick(); };

        Shown += delegate
        {
            BringToFront();
            composer.Focus();
            composer.Select(6, 5);
            timer.Start();
        };
    }

    void Tick()
    {
        // Take the foreground once at startup: the test launched us from a background
        // process, which Windows' foreground lock may not let us replace. A synthetic Alt
        // press is the documented way to release the lock for the next SetForegroundWindow.
        if (activateTries < 40 && GetForegroundWindow() != Handle)
        {
            activateTries++;
            keybd_event(0x12, 0, 0, UIntPtr.Zero);
            keybd_event(0x12, 0, 2, UIntPtr.Zero);
            SetForegroundWindow(Handle);
            Activate();
            composer.Focus();
        }
        else if (activateTries < 40)
        {
            activateTries = 40;
        }
        WriteState();
    }

    static string Json(string s)
    {
        StringBuilder b = new StringBuilder("\"");
        foreach (char c in s)
        {
            switch (c)
            {
                case '"': b.Append("\\\""); break;
                case '\\': b.Append("\\\\"); break;
                case '\n': b.Append("\\n"); break;
                case '\r': b.Append("\\r"); break;
                case '\t': b.Append("\\t"); break;
                default:
                    if (c < ' ') b.Append(string.Format("\\u{0:x4}", (int)c));
                    else b.Append(c);
                    break;
            }
        }
        return b.Append('"').ToString();
    }

    void WriteState()
    {
        string selected = fruits.SelectedItem == null ? "null" : Json(fruits.SelectedItem.ToString());
        string focus = composer.Focused ? "composer" : nameBox.Focused ? "name" : apply.Focused
            ? "apply" : notifications.Focused ? "notifications" : fruits.Focused ? "fruits" : "";
        string state = "{"
            + "\"pid\":" + System.Diagnostics.Process.GetCurrentProcess().Id
            + ",\"title\":" + Json(Text)
            + ",\"composer\":" + Json(composer.Text.Replace("\r\n", "\n"))
            + ",\"selection\":[" + composer.SelectionStart + ","
            + (composer.SelectionStart + composer.SelectionLength) + "]"
            + ",\"name\":" + Json(nameBox.Text)
            + ",\"checked\":" + (notifications.Checked ? "true" : "false")
            + ",\"fruit\":" + selected
            + ",\"status\":" + Json(status.Text)
            + ",\"presses\":" + presses
            + ",\"focus\":" + Json(focus)
            + ",\"foreground\":" + (GetForegroundWindow() == Handle ? "true" : "false")
            + "}";
        if (state == lastState) return;
        try
        {
            string tmp = statePath + ".tmp";
            File.WriteAllText(tmp, state, new UTF8Encoding(false));
            if (File.Exists(statePath)) File.Replace(tmp, statePath, null);
            else File.Move(tmp, statePath);
            lastState = state;
        }
        catch (IOException)
        {
            // The test was reading it; try again on the next tick.
        }
        catch (UnauthorizedAccessException)
        {
        }
    }
}

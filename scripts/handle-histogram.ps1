<#
.SYNOPSIS
    Prints a process's open handles grouped by kernel object type.

.DESCRIPTION
    A raw HandleCount says a process leaks; it never says what. This walks the
    system handle table (SystemExtendedHandleInformation) and names each entry
    from the kernel's own type table (NtQueryObject/ObjectTypesInformation), so
    the answer is "12 Events and 3 ALPC Ports" rather than "+15".

    This is the tool that attributed the arm/disarm handle leak on 2026-08-01.
    A process-wide count could not distinguish the capture session (which leaks
    only ALPC Ports) from the encoder (Event + WaitCompletionPacket + Key); the
    per-type split is what pointed at the driver stack instead of at Trix. See
    the header of arm-cycle-leak.ps1 for the finding, and
    crates/trix-core/tests/handle_leak.rs for the per-subsystem cycling.

    Deliberately does NOT call NtQueryObject on individual handles. Querying a
    synchronous named-pipe handle whose peer is not reading blocks forever, and
    the daemon owns exactly such a pipe. The type table is queried once with a
    NULL handle, which cannot block.

    Self-checks its own struct offsets before reporting: it creates an Event in
    this process and asserts the walk names that handle "Event". Wrong offsets
    otherwise produce confident, plausible garbage -- during development they
    silently attributed every handle to the wrong type, because the object type
    index sits at offset 30 and CreatorBackTraceIndex (usually 0) sits at 28.

.PARAMETER ProcessId
    The process to inspect. Needs no special privilege for a process owned by
    the same user.

.EXAMPLE
    .\handle-histogram.ps1 -ProcessId (Get-Process trix-daemon).Id -Label armed
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][int]$ProcessId,
    [string]$Label = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if (-not ('Trix.HandleProbe' -as [type])) {
Add-Type -Language CSharp @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;

namespace Trix {
  public static class HandleProbe {
    [DllImport("ntdll.dll")]
    static extern int NtQuerySystemInformation(int cls, IntPtr buf, int len, out int ret);
    [DllImport("ntdll.dll")]
    static extern int NtQueryObject(IntPtr h, int cls, IntPtr buf, int len, out int ret);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern IntPtr CreateEventW(IntPtr sa, bool manual, bool initial, IntPtr name);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool CloseHandle(IntPtr h);

    const int SystemExtendedHandleInformation = 64;
    const int ObjectTypesInformation = 3;
    const int STATUS_INFO_LENGTH_MISMATCH = unchecked((int)0xC0000004);

    // Kernel type table: index -> name. Queried once, with a NULL handle.
    static Dictionary<int, string> TypeNames() {
      int len = 64 * 1024;
      IntPtr buf = IntPtr.Zero;
      try {
        while (true) {
          buf = Marshal.AllocHGlobal(len);
          int ret;
          int st = NtQueryObject(IntPtr.Zero, ObjectTypesInformation, buf, len, out ret);
          if (st == STATUS_INFO_LENGTH_MISMATCH) {
            Marshal.FreeHGlobal(buf); buf = IntPtr.Zero;
            len = Math.Max(ret, len * 2);
            continue;
          }
          if (st < 0) throw new Exception("NtQueryObject(ObjectTypesInformation) failed 0x" + st.ToString("X8"));
          break;
        }
        var map = new Dictionary<int, string>();
        int count = Marshal.ReadInt32(buf);
        // Entries follow ULONG NumberOfTypes, aligned to pointer size.
        IntPtr p = new IntPtr(buf.ToInt64() + IntPtr.Size);
        for (int i = 0; i < count; i++) {
          // OBJECT_TYPE_INFORMATION: UNICODE_STRING TypeName at 0
          // (Length, MaximumLength, then Buffer at 8 once padded), TypeIndex at 90,
          // total size 104 on x64.
          short nameLen = Marshal.ReadInt16(p, 0);
          short nameMax = Marshal.ReadInt16(p, 2);
          IntPtr nameBuf = Marshal.ReadIntPtr(p, 8);
          string name = (nameBuf != IntPtr.Zero && nameLen > 0)
            ? Marshal.PtrToStringUni(nameBuf, nameLen / 2) : "<unnamed>";
          int typeIndex = Marshal.ReadByte(p, 90);
          map[typeIndex] = name;
          long next = p.ToInt64() + 104 + nameMax;
          next = (next + IntPtr.Size - 1) & ~((long)IntPtr.Size - 1);
          p = new IntPtr(next);
        }
        return map;
      } finally { if (buf != IntPtr.Zero) Marshal.FreeHGlobal(buf); }
    }

    // SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX, 40 bytes on x64:
    //   Object(8) UniqueProcessId(8) HandleValue(8) GrantedAccess(4)
    //   CreatorBackTraceIndex(2) ObjectTypeIndex(2) HandleAttributes(4) Reserved(4)
    // The non-extended class is NOT usable here: its UniqueProcessId is a
    // USHORT, which silently truncates any PID above 65535.
    static List<KeyValuePair<long, int>> Handles(long pid) {
      int len = 4 * 1024 * 1024;
      IntPtr buf = IntPtr.Zero;
      try {
        while (true) {
          buf = Marshal.AllocHGlobal(len);
          int ret;
          int st = NtQuerySystemInformation(SystemExtendedHandleInformation, buf, len, out ret);
          if (st == STATUS_INFO_LENGTH_MISMATCH) {
            Marshal.FreeHGlobal(buf); buf = IntPtr.Zero;
            len = Math.Max(ret + 256 * 1024, len * 2);
            continue;
          }
          if (st < 0) throw new Exception("NtQuerySystemInformation failed 0x" + st.ToString("X8"));
          break;
        }
        long n = Marshal.ReadIntPtr(buf).ToInt64();
        IntPtr p = new IntPtr(buf.ToInt64() + IntPtr.Size * 2);
        var outp = new List<KeyValuePair<long, int>>();
        for (long i = 0; i < n; i++) {
          IntPtr e = new IntPtr(p.ToInt64() + i * 40);
          long owner = Marshal.ReadIntPtr(e, 8).ToInt64();
          if (owner != pid) continue;
          long handle = Marshal.ReadIntPtr(e, 16).ToInt64();
          int typeIndex = (ushort)Marshal.ReadInt16(e, 30);
          outp.Add(new KeyValuePair<long, int>(handle, typeIndex));
        }
        return outp;
      } finally { if (buf != IntPtr.Zero) Marshal.FreeHGlobal(buf); }
    }

    /// Returns "TypeName=count" lines. Throws if the self-check fails.
    public static string[] Histogram(int pid) {
      var names = TypeNames();

      // Falsify the walk before trusting it: an Event we just created must come
      // back named "Event". The two ways this can fail are reported separately
      // -- one message for both cost a debugging round.
      IntPtr probe = CreateEventW(IntPtr.Zero, true, false, IntPtr.Zero);
      if (probe == IntPtr.Zero) throw new Exception("CreateEvent failed for self-check");
      try {
        long self = System.Diagnostics.Process.GetCurrentProcess().Id;
        string got = null;
        bool seen = false;
        int idx = -1;
        foreach (var kv in Handles(self))
          if (kv.Key == probe.ToInt64()) { seen = true; idx = kv.Value; names.TryGetValue(idx, out got); break; }
        if (!seen)
          throw new Exception("self-check failed: the handle-table walk did not find our own Event handle ("
                              + names.Count + " types known)");
        if (got != "Event")
          throw new Exception("self-check failed: our own Event handle has type index " + idx
                              + ", which the type table names '" + (got ?? "<absent>") + "'");
      } finally { CloseHandle(probe); }

      var counts = new SortedDictionary<string, int>(StringComparer.Ordinal);
      foreach (var kv in Handles(pid)) {
        string name;
        if (!names.TryGetValue(kv.Value, out name)) name = "type#" + kv.Value;
        counts[name] = counts.ContainsKey(name) ? counts[name] + 1 : 1;
      }
      var lines = new List<string>();
      foreach (var kv in counts) lines.Add(kv.Key + "=" + kv.Value);
      return lines.ToArray();
    }
  }
}
'@
}

$lines = [Trix.HandleProbe]::Histogram($ProcessId)
$total = 0
$rows = foreach ($l in $lines) {
    $k, $v = $l -split '=', 2
    $total += [int]$v
    [PSCustomObject]@{ Type = $k; Count = [int]$v }
}
if ($Label) { "--- $Label (pid $ProcessId) ---" }
$rows | Sort-Object Count -Descending | Format-Table -AutoSize | Out-String -Width 60
"total handles: $total"

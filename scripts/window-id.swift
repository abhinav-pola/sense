// Prints the CoreGraphics window number of a process's main window, for
// `screencapture -l`: e2e checks what the real window shows, which offscreen
// renders don't always match. usage: window-id <pid>
import CoreGraphics
let pid = Int32(CommandLine.arguments[1])!
let list = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as! [[String: Any]]
for w in list where (w[kCGWindowOwnerPID as String] as? Int32) == pid {
    if let b = w[kCGWindowBounds as String] as? [String: Any], (b["Width"] as? Double ?? 0) > 500 {
        print(w[kCGWindowNumber as String]!); break
    }
}

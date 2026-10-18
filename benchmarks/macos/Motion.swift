import AppKit
import QuartzCore

@MainActor
final class MotionDelegate: NSObject, NSApplicationDelegate {
    var window: NSWindow?

    func applicationDidFinishLaunching(_ notification: Notification) {
        guard let screen = NSScreen.screens.max(by: {
            $0.frame.width * $0.frame.height < $1.frame.width * $1.frame.height
        }) else { NSApp.terminate(nil); return }
        let window = NSWindow(contentRect: screen.visibleFrame,
                              styleMask: [.titled, .closable, .resizable],
                              backing: .buffered, defer: false)
        window.title = "iShareScreen synthetic motion — closes automatically after 60 seconds"
        window.isReleasedWhenClosed = false
        let view = NSView(frame: NSRect(origin: .zero, size: screen.visibleFrame.size))
        view.wantsLayer = true
        let background = CAGradientLayer()
        background.frame = view.bounds
        background.colors = [NSColor.darkGray.cgColor, NSColor.black.cgColor]
        view.layer = background
        for i in 0..<12 {
            let bar = CALayer()
            let height = view.bounds.height / 12
            bar.frame = NSRect(x: -view.bounds.width * 0.4,
                               y: CGFloat(i) * height,
                               width: view.bounds.width * 0.65, height: height - 2)
            bar.backgroundColor = NSColor(calibratedHue: CGFloat(i)/12,
                                          saturation: 0.8, brightness: 0.9, alpha: 1).cgColor
            background.addSublayer(bar)
            let animation = CABasicAnimation(keyPath: "transform.translation.x")
            animation.fromValue = 0
            animation.toValue = view.bounds.width * 1.1
            animation.duration = 1.5 + Double(i) * 0.03
            animation.autoreverses = true
            animation.repeatCount = .infinity
            animation.timingFunction = CAMediaTimingFunction(name: .linear)
            bar.add(animation, forKey: "motion")
        }
        let label = NSTextField(labelWithString: "Synthetic motion test • GPU decode / presentation benchmark\nClose this window to stop. It exits automatically after 60 seconds.")
        label.font = .systemFont(ofSize: 25, weight: .semibold)
        label.textColor = .white
        label.backgroundColor = .black
        label.drawsBackground = true
        label.frame = NSRect(x: 24, y: view.bounds.height - 100,
                             width: view.bounds.width - 48, height: 76)
        view.addSubview(label)
        window.contentView = view
        self.window = window
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        Timer.scheduledTimer(timeInterval: 60, target: self,
                             selector: #selector(finish), userInfo: nil, repeats: false)
    }

    @objc func finish() { NSApp.terminate(nil) }
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}

@main
struct MotionBenchmark {
    @MainActor static func main() {
        let app = NSApplication.shared
        app.setActivationPolicy(.regular)
        let delegate = MotionDelegate()
        app.delegate = delegate
        withExtendedLifetime(delegate) { app.run() }
    }
}

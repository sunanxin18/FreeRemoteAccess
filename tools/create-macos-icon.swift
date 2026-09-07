import AppKit
import Foundation

// 从已审核的无蒙版 Apple 图层生成原生 ICNS，不预制圆角。
guard CommandLine.arguments.count == 3 else { fatalError("需要图层目录和 iconset 输出目录") }
let source = URL(fileURLWithPath: CommandLine.arguments[1])
let output = URL(fileURLWithPath: CommandLine.arguments[2])
try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
guard let background = NSImage(contentsOf: source.appendingPathComponent("background.png")),
      let foreground = NSImage(contentsOf: source.appendingPathComponent("foreground.png")) else {
    fatalError("缺少 Apple 图标图层")
}
for pointSize in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let pixels = pointSize * scale
        guard let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: pixels,
            pixelsHigh: pixels, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true,
            isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0),
              let context = NSGraphicsContext(bitmapImageRep: bitmap) else {
            fatalError("无法创建图标位图")
        }
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = context
        context.imageInterpolation = .high
        let rect = NSRect(x: 0, y: 0, width: pixels, height: pixels)
        background.draw(in: rect)
        foreground.draw(in: rect)
        context.flushGraphics()
        NSGraphicsContext.restoreGraphicsState()
        let suffix = scale == 2 ? "@2x" : ""
        guard let png = bitmap.representation(using: .png, properties: [:]) else { fatalError("无法编码图标") }
        try png.write(to: output.appendingPathComponent("icon_\(pointSize)x\(pointSize)\(suffix).png"))
    }
}

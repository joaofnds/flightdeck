import Foundation

func schematicReadings(selection: Selection?, fans: SelectedFanReadings?) -> SchematicReadings {
    guard let selected = selection?.selected else { return SchematicReadings() }

    var readings = SchematicReadings()
    if let summary = selection?.summary, summary.identity == selected.identity,
       summary.wallNs == selected.wallNs {
        readings.cpuPercent = summary.cpuPercent
        readings.gpuPercent = summary.gpuActivePercent
        readings.memoryTotalGiB = summary.memoryTotalBytes.map { Double($0) / 1_073_741_824 }
        readings.batteryPercent = summary.battery?.chargePercent
        readings.batteryWatts = summary.battery?.powerWatts
    }
    if let fans, fans.observation.identity == selected.identity,
       fans.observation.runTick == selected.runTick, fans.observation.wallNs == selected.wallNs {
        readings.fan0 = fans.fan0
        readings.fan1 = fans.fan1
    }

    return readings
}

@MainActor
extension RecordingModel {
    var schematicPreviewSource: SchematicPreviewSource {
        SchematicPreviewSource(path: path, live: isLiveSource,
            run: selection?.selected?.run, boot: selection?.selected?.boot)
    }
}

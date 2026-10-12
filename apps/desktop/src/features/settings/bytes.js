/**
 * Honest byte formatting for storage disclosure (§4.1 "disclose disk
 * requirements"). `null` means unknown and is never rendered as 0.
 */
const UNITS = [
    ["TB", 1024 ** 4],
    ["GB", 1024 ** 3],
    ["MB", 1024 ** 2],
    ["kB", 1024],
];
export function formatBytes(bytes) {
    if (bytes === null || !Number.isFinite(bytes))
        return "unknown";
    if (bytes < 1024)
        return `${Math.round(bytes)} bytes`;
    for (const [label, size] of UNITS) {
        if (bytes >= size) {
            const value = bytes / size;
            const digits = value < 10 ? 1 : 0;
            return `${value.toFixed(digits)} ${label}`;
        }
    }
    return `${Math.round(bytes)} bytes`;
}

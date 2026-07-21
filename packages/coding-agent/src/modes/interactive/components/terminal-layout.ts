import { truncateToWidth, visibleWidth } from "@frelion/bone-tui";

/**
 * Terminal-safe layout primitives. ANSI escape sequences must never be passed
 * to String.padEnd(), because they consume JS characters but no terminal cells.
 */
export function fitLine(text: string, width: number, ellipsis = ""): string {
	if (width <= 0) return "";
	const truncated = truncateToWidth(text, width, ellipsis);
	return `${truncated}${" ".repeat(Math.max(0, width - visibleWidth(truncated)))}`;
}

export function joinColumns(
	left: string,
	leftWidth: number,
	separator: string,
	right: string,
	rightWidth: number,
): string {
	return `${fitLine(left, leftWidth)}${separator}${fitLine(right, rightWidth)}`;
}

export function alignEnd(left: string, right: string, width: number, gap = 1): string {
	const availableLeft = Math.max(0, width - visibleWidth(right) - gap);
	const fittedLeft = truncateToWidth(left, availableLeft, "");
	const spacing = Math.max(gap, width - visibleWidth(fittedLeft) - visibleWidth(right));
	return fitLine(`${fittedLeft}${" ".repeat(spacing)}${right}`, width);
}

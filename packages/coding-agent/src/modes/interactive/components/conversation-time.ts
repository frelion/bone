function calendarDayDifference(earlier: Date, later: Date): number {
	const earlierDay = new Date(earlier.getFullYear(), earlier.getMonth(), earlier.getDate()).getTime();
	const laterDay = new Date(later.getFullYear(), later.getMonth(), later.getDate()).getTime();
	return Math.round((laterDay - earlierDay) / 86_400_000);
}

function formatTimeOfDay(date: Date): string {
	return `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
}

function formatCalendarDate(date: Date, now: Date): string {
	const options: Intl.DateTimeFormatOptions =
		date.getFullYear() === now.getFullYear()
			? { month: "short", day: "numeric" }
			: { month: "short", day: "numeric", year: "numeric" };
	return new Intl.DateTimeFormat("en-US", options).format(date);
}

export function formatConversationActivityTime(date: Date, now = new Date()): string {
	if (Number.isNaN(date.getTime())) return "-";
	const elapsed = Math.max(0, now.getTime() - date.getTime());
	if (elapsed < 60_000) return "now";
	if (elapsed < 60 * 60_000) return `${Math.floor(elapsed / 60_000)}m`;
	const daysAgo = calendarDayDifference(date, now);
	if (daysAgo === 0) return formatTimeOfDay(date);
	if (daysAgo === 1) return "yesterday";
	if (daysAgo > 1 && daysAgo < 7) return new Intl.DateTimeFormat("en-US", { weekday: "short" }).format(date);
	return formatCalendarDate(date, now);
}

export function formatConversationCreatedTime(date: Date, now = new Date()): string {
	if (Number.isNaN(date.getTime())) return "created unknown";
	const daysAgo = calendarDayDifference(date, now);
	if (daysAgo === 0) return `created ${formatTimeOfDay(date)}`;
	if (daysAgo === 1) return "created yesterday";
	return `created ${formatCalendarDate(date, now)}`;
}

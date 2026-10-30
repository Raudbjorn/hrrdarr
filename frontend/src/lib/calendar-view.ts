// Pure Calendar view helpers: local-date arithmetic, visible ranges, query strings, entry
// normalization, status labels and keyboard movement. No DOM or network access; callers render
// every string as text. Local dates are `YYYY-MM-DD` strings computed with UTC arithmetic so
// DST transitions never skip or repeat a day; only TV instants are converted to local time.
import type {
	MediaDomain,
	MovieCalendarEntry,
	MovieReleaseType,
	TvCalendarEntry,
} from "./api.generated";

export type CalendarView = "month" | "week" | "agenda";
export type CalendarScope = MediaDomain | "both";
export type LocalDate = string;
export type DateRange = { first: LocalDate; end: LocalDate };
export type CalendarOptions = {
	unmonitored: boolean;
	specials: boolean;
	releaseTypes: MovieReleaseType[];
};
export type CalendarStatus =
	| "downloaded"
	| "missing"
	| "airing"
	| "unaired"
	| "upcoming"
	| "unmonitored";
export type CalendarItem = {
	key: string;
	domain: MediaDomain;
	date: LocalDate;
	time: string | null;
	sort: number;
	title: string;
	detail: string;
	releaseType: MovieReleaseType | null;
	status: CalendarStatus;
	statusLabel: string;
	targetId: number;
};

/** Sunday-first weeks, as in the reference default. */
export const WEEK_START = 0;
export const MAX_RANGE_DAYS = 366;
export const POLL_INTERVAL_MS = 60_000;
export const FEED_PAST_DAYS = 7;
export const FEED_FUTURE_DAYS = 28;
export const RELEASE_TYPES: readonly MovieReleaseType[] = [
	"in_cinemas",
	"digital",
	"physical",
];
export const RELEASE_LABELS: Readonly<Record<MovieReleaseType, string>> = {
	in_cinemas: "In cinemas",
	digital: "Digital release",
	physical: "Physical release",
};
export const STATUS_LABELS: Readonly<Record<CalendarStatus, string>> = {
	downloaded: "Downloaded",
	missing: "Missing",
	airing: "On air",
	unaired: "Not aired yet",
	upcoming: "Not released yet",
	unmonitored: "Unmonitored",
};
export const WEEKDAYS = [
	"Sunday",
	"Monday",
	"Tuesday",
	"Wednesday",
	"Thursday",
	"Friday",
	"Saturday",
] as const;
const MONTHS = [
	"January",
	"February",
	"March",
	"April",
	"May",
	"June",
	"July",
	"August",
	"September",
	"October",
	"November",
	"December",
] as const;
const DAY_MS = 86_400_000;
const pad = (value: number, width = 2) =>
	String(Math.trunc(Math.abs(value))).padStart(width, "0");

export function defaultOptions(): CalendarOptions {
	return {
		unmonitored: false,
		specials: true,
		releaseTypes: [...RELEASE_TYPES],
	};
}

/** Strict `YYYY-MM-DD` (years 1–9999, real calendar day). */
export function parseLocalDate(
	value: string,
): { year: number; month: number; day: number } | null {
	const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
	if (!match) return null;
	const [year, month, day] = [
		Number(match[1]),
		Number(match[2]),
		Number(match[3]),
	];
	if (year < 1 || month < 1 || month > 12 || day < 1) return null;
	const probe = new Date(Date.UTC(2000, month - 1, day));
	probe.setUTCFullYear(year);
	if (
		probe.getUTCFullYear() !== year ||
		probe.getUTCMonth() !== month - 1 ||
		probe.getUTCDate() !== day
	)
		return null;
	return { year, month, day };
}
function utcMs(date: LocalDate): number {
	const parts = parseLocalDate(date);
	if (!parts) throw new RangeError(`Invalid local date: ${date}`);
	const probe = new Date(Date.UTC(2000, parts.month - 1, parts.day));
	probe.setUTCFullYear(parts.year);
	return probe.getTime();
}
function fromUtcMs(ms: number): LocalDate {
	const d = new Date(ms);
	return `${pad(d.getUTCFullYear(), 4)}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())}`;
}
export function addDays(date: LocalDate, days: number): LocalDate {
	return fromUtcMs(utcMs(date) + Math.trunc(days) * DAY_MS);
}
export function daysBetween(first: LocalDate, end: LocalDate): number {
	return Math.round((utcMs(end) - utcMs(first)) / DAY_MS);
}
/** Same day of a different month, clamped to that month's last day. */
export function addMonths(date: LocalDate, months: number): LocalDate {
	const p = parseLocalDate(date);
	if (!p) throw new RangeError(`Invalid local date: ${date}`);
	const index = p.year * 12 + (p.month - 1) + Math.trunc(months);
	const year = Math.floor(index / 12),
		month = index - year * 12 + 1;
	const first = `${pad(year, 4)}-${pad(month)}-01`;
	return `${first.slice(0, 8)}${pad(Math.min(p.day, daysBetween(first, nextMonth(first))))}`;
}
function nextMonth(first: LocalDate): LocalDate {
	const year = Number(first.slice(0, 4)),
		month = Number(first.slice(5, 7));
	return month === 12
		? `${pad(year + 1, 4)}-01-01`
		: `${pad(year, 4)}-${pad(month + 1)}-01`;
}
export function weekday(date: LocalDate): number {
	return new Date(utcMs(date)).getUTCDay();
}
export function startOfWeek(date: LocalDate): LocalDate {
	return addDays(date, -((weekday(date) - WEEK_START + 7) % 7));
}
export function startOfMonth(date: LocalDate): LocalDate {
	return `${date.slice(0, 8)}01`;
}
/** The viewer's local civil date for an instant. */
export function localDateOf(instant: Date): LocalDate {
	return `${pad(instant.getFullYear(), 4)}-${pad(instant.getMonth() + 1)}-${pad(instant.getDate())}`;
}

/** Visible dates `[first, end)`: month grids include adjacent days to complete their weeks. */
export function rangeFor(view: CalendarView, anchor: LocalDate): DateRange {
	if (view === "week") {
		const first = startOfWeek(anchor);
		return { first, end: addDays(first, 7) };
	}
	const monthStart = startOfMonth(anchor),
		next = nextMonth(monthStart);
	if (view === "agenda") return { first: monthStart, end: next };
	const first = startOfWeek(monthStart),
		lastWeek = startOfWeek(addDays(next, -1));
	return { first, end: addDays(lastWeek, 7) };
}
export function weeksOf(range: DateRange): LocalDate[][] {
	const weeks: LocalDate[][] = [];
	for (
		let day = range.first;
		daysBetween(day, range.end) > 0;
		day = addDays(day, 7)
	) {
		weeks.push(Array.from({ length: 7 }, (_, i) => addDays(day, i)));
	}
	return weeks;
}
/** Previous/next step for the view: one week, or one month for month and agenda views. */
export function step(
	view: CalendarView,
	anchor: LocalDate,
	direction: -1 | 1,
): LocalDate {
	return view === "week"
		? addDays(anchor, 7 * direction)
		: addMonths(anchor, direction);
}
export function monthName(date: LocalDate): string {
	return `${MONTHS[Number(date.slice(5, 7)) - 1]} ${Number(date.slice(0, 4))}`;
}
export function dateLabel(date: LocalDate): string {
	return `${WEEKDAYS[weekday(date)]}, ${MONTHS[Number(date.slice(5, 7)) - 1]} ${Number(date.slice(8))}, ${Number(date.slice(0, 4))}`;
}
export function rangeTitle(view: CalendarView, anchor: LocalDate): string {
	if (view !== "week") return monthName(anchor);
	const { first, end } = rangeFor("week", anchor),
		last = addDays(end, -1);
	const short = (d: LocalDate) =>
		`${MONTHS[Number(d.slice(5, 7)) - 1].slice(0, 3)} ${Number(d.slice(8))}`;
	return `Week of ${short(first)} – ${short(last)}, ${Number(last.slice(0, 4))}`;
}

/** Local midnight of `date` as an RFC 3339 instant with the viewer's offset at that moment. */
export function localMidnight(date: LocalDate): string {
	const p = parseLocalDate(date);
	if (!p) throw new RangeError(`Invalid local date: ${date}`);
	const at = new Date(2000, p.month - 1, p.day);
	at.setFullYear(p.year);
	const offset = -at.getTimezoneOffset();
	const sign = offset < 0 ? "-" : "+";
	// A zone whose midnight does not exist (DST gap) yields the first existing local time.
	return `${pad(at.getFullYear(), 4)}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}T${pad(at.getHours())}:${pad(at.getMinutes())}:${pad(at.getSeconds())}${sign}${pad(offset / 60)}:${pad(offset % 60)}`;
}
/** TV: local-midnight instants, so each air time lands on the viewer's local day. */
export function tvQuery(range: DateRange, options: CalendarOptions): string {
	const params = new URLSearchParams({
		start: localMidnight(range.first),
		end: localMidnight(range.end),
	});
	if (options.unmonitored) params.set("unmonitored", "true");
	if (!options.specials) params.set("specials", "false");
	return params.toString();
}
/** Movies: plain dates, because release dates are floating all-day dates. `null` when no
 * release type is selected (the server refuses an empty set). */
export function movieQuery(
	range: DateRange,
	options: CalendarOptions,
): string | null {
	const types = RELEASE_TYPES.filter((type) =>
		options.releaseTypes.includes(type),
	);
	if (!types.length) return null;
	const params = new URLSearchParams({ start: range.first, end: range.end });
	if (options.unmonitored) params.set("unmonitored", "true");
	if (types.length < RELEASE_TYPES.length)
		params.set("release_types", types.join(","));
	return params.toString();
}
export function feedPath(
	domain: MediaDomain,
	options: CalendarOptions,
): string | null {
	const params = new URLSearchParams({
		past_days: String(FEED_PAST_DAYS),
		future_days: String(FEED_FUTURE_DAYS),
	});
	if (options.unmonitored) params.set("unmonitored", "true");
	if (domain === "tv" && !options.specials) params.set("specials", "false");
	if (domain === "movies") {
		const types = RELEASE_TYPES.filter((type) =>
			options.releaseTypes.includes(type),
		);
		if (!types.length) return null;
		if (types.length < RELEASE_TYPES.length)
			params.set("release_types", types.join(","));
	}
	return `/feed/v1/calendar/${domain}.ics?${params.toString()}`;
}
/** `webcal:` subscription form of an HTTP(S) feed URL; other schemes are refused. */
export function webcalUrl(httpUrl: string): string | null {
	return /^https?:\/\//.test(httpUrl)
		? httpUrl.replace(/^https?:/, "webcal:")
		: null;
}

export function episodeCode(entry: TvCalendarEntry): string {
	if (entry.series_type === "daily") return entry.air_date ?? "Daily";
	if (entry.season === 0) return `Special ${entry.number}`;
	const code = `S${pad(entry.season)}E${pad(entry.number)}`;
	return entry.series_type === "anime" && entry.absolute_episode_number !== null
		? `${code} (${entry.absolute_episode_number})`
		: code;
}
export function tvStatus(entry: TvCalendarEntry, now: number): CalendarStatus {
	if (entry.has_file) return "downloaded";
	if (!entry.effective_monitored) return "unmonitored";
	const start = Date.parse(entry.starts_at),
		end = entry.ends_at ? Date.parse(entry.ends_at) : start;
	if (start > now) return "unaired";
	if (now < end) return "airing";
	return "missing";
}
export function movieStatus(
	entry: MovieCalendarEntry,
	today: LocalDate,
): CalendarStatus {
	if (entry.has_file) return "downloaded";
	if (!entry.monitored) return "unmonitored";
	return daysBetween(today, entry.date) > 0 ? "upcoming" : "missing";
}
export function tvItem(entry: TvCalendarEntry, now: number): CalendarItem {
	const at = new Date(entry.starts_at),
		status = tvStatus(entry, now);
	return {
		key: `tv:${entry.event_id}`,
		domain: "tv",
		date: localDateOf(at),
		time: `${pad(at.getHours())}:${pad(at.getMinutes())}`,
		sort: at.getTime(),
		title: entry.series_title,
		detail: `${episodeCode(entry)} · ${entry.title}`,
		releaseType: null,
		status,
		statusLabel: STATUS_LABELS[status],
		targetId: entry.target.id,
	};
}
export function movieItem(
	entry: MovieCalendarEntry,
	today: LocalDate,
): CalendarItem {
	const status = movieStatus(entry, today);
	return {
		key: `movies:${entry.event_id}`,
		domain: "movies",
		date: entry.date,
		time: null,
		// All-day releases sort before timed entries on the same day, in release-type order.
		sort:
			utcMs(entry.date) - DAY_MS + RELEASE_TYPES.indexOf(entry.release_type),
		title: entry.year ? `${entry.title} (${entry.year})` : entry.title,
		detail: RELEASE_LABELS[entry.release_type],
		releaseType: entry.release_type,
		status,
		statusLabel:
			status === "missing" ? "Released, no file" : STATUS_LABELS[status],
		targetId: entry.target.id,
	};
}
/** Items grouped by local date, each day sorted by time then key; only dates in range. */
export function groupByDate(
	items: readonly CalendarItem[],
	range: DateRange,
): Map<LocalDate, CalendarItem[]> {
	const out = new Map<LocalDate, CalendarItem[]>();
	for (const item of [...items].sort(
		(a, b) =>
			a.date.localeCompare(b.date) ||
			a.sort - b.sort ||
			a.key.localeCompare(b.key),
	)) {
		if (item.date < range.first || item.date >= range.end) continue;
		const day = out.get(item.date);
		if (day) day.push(item);
		else out.set(item.date, [item]);
	}
	return out;
}
/** Grid keyboard movement (APG date-grid pattern); `null` for keys the grid does not own. */
export function moveDate(date: LocalDate, key: string): LocalDate | null {
	switch (key) {
		case "ArrowLeft":
			return addDays(date, -1);
		case "ArrowRight":
			return addDays(date, 1);
		case "ArrowUp":
			return addDays(date, -7);
		case "ArrowDown":
			return addDays(date, 7);
		case "Home":
			return startOfWeek(date);
		case "End":
			return addDays(startOfWeek(date), 6);
		case "PageUp":
			return addMonths(date, -1);
		case "PageDown":
			return addMonths(date, 1);
		default:
			return null;
	}
}
export function inRange(date: LocalDate, range: DateRange): boolean {
	return date >= range.first && date < range.end;
}
/** The focused day of a grid: the focus when it is inside the rendered range, otherwise the
 * anchor, which every view's range contains. A view switch therefore keeps one tabbable day. */
export function focusInRange(
	view: CalendarView,
	anchor: LocalDate,
	focus: LocalDate,
): LocalDate {
	return inRange(focus, rangeFor(view, anchor)) ? focus : anchor;
}
export function countLabel(count: number): string {
	return count === 0
		? "nothing scheduled"
		: `${count} ${count === 1 ? "entry" : "entries"}`;
}

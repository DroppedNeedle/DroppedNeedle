// How the year in review prints its numbers and dates. The server sends
// days as ISO dates already in the year's time zone, so they are read as
// plain calendar dates, never shifted into another zone.

const MONTHS = ['JAN', 'FEB', 'MAR', 'APR', 'MAY', 'JUN', 'JUL', 'AUG', 'SEP', 'OCT', 'NOV', 'DEC'];
const MONTH_NAMES = [
	'January',
	'February',
	'March',
	'April',
	'May',
	'June',
	'July',
	'August',
	'September',
	'October',
	'November',
	'December'
];
const WEEKDAYS = ['SUN', 'MON', 'TUE', 'WED', 'THU', 'FRI', 'SAT'];

/** The lists and the month row need this much before they show. */
export const MIN_LIST = 3;
export const MIN_MONTHS = 2;

export const count = (n: number): string => n.toLocaleString();

/** Whole hours of the songs played. */
export const hours = (ms: number): number => Math.floor(ms / 3_600_000);

/** A list rank as printed beside a cover: "02". */
export const rank = (place: number): string => String(place).padStart(2, '0');

export const plays = (n: number): string => `${count(n)} ${n === 1 ? 'PLAY' : 'PLAYS'}`;

/** The month's short caps name, 1 for January. */
export const monthShort = (month: number): string => MONTHS[month - 1] ?? '';

/** The month's full name, 1 for January. */
export const monthName = (month: number): string => MONTH_NAMES[month - 1] ?? '';

function parts(iso: string): { year: number; month: number; day: number } | null {
	const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(iso);
	if (!match) return null;
	return { year: Number(match[1]), month: Number(match[2]), day: Number(match[3]) };
}

/** "3 FEB". */
export function day(iso: string): string {
	const date = parts(iso);
	return date ? `${date.day} ${monthShort(date.month)}` : iso;
}

/** "SAT 14 JUN". */
export function weekday(iso: string): string {
	const date = parts(iso);
	if (!date) return iso;
	const dow = new Date(Date.UTC(date.year, date.month - 1, date.day)).getUTCDay();
	return `${WEEKDAYS[dow]} ${date.day} ${monthShort(date.month)}`;
}

/** The month (1 for January) of an ISO date. */
export function monthOf(iso: string): number | null {
	return parts(iso)?.month ?? null;
}

/** "1 JAN 00:14": a moment, in the zone the year was counted in. */
export function moment(ms: number, zone: string): string {
	const options: Intl.DateTimeFormatOptions = {
		day: 'numeric',
		month: 'numeric',
		hour: '2-digit',
		minute: '2-digit',
		hourCycle: 'h23'
	};
	let format: Intl.DateTimeFormat;
	try {
		format = new Intl.DateTimeFormat('en-GB', { ...options, timeZone: zone });
	} catch {
		format = new Intl.DateTimeFormat('en-GB', options);
	}
	const bits = Object.fromEntries(format.formatToParts(ms).map((p) => [p.type, p.value]));
	return `${bits.day} ${monthShort(Number(bits.month))} ${bits.hour}:${bits.minute}`;
}

/** The address of a year's page: the current year has the plain one. */
export function yearPath(year: number, current: number): string {
	return year === current ? '/your-year' : `/your-year/${year}`;
}

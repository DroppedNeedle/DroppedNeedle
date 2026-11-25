// The week's dates and counts in the words the counter prints them in.

function parts(ms: number) {
	const date = new Date(ms);
	return {
		weekday: date.toLocaleDateString(undefined, { weekday: 'short' }),
		weekdayLong: date.toLocaleDateString(undefined, { weekday: 'long' }),
		day: date.getDate(),
		month: date.toLocaleDateString(undefined, { month: 'short' }),
		year: date.getFullYear()
	};
}

/** "Fri 20 Nov", from unix milliseconds. */
export function dayDate(ms: number): string {
	const { weekday, day, month } = parts(ms);
	return `${weekday} ${day} ${month}`;
}

/** "Friday 27 Nov", from unix milliseconds. */
export function longDayDate(ms: number): string {
	const { weekdayLong, day, month } = parts(ms);
	return `${weekdayLong} ${day} ${month}`;
}

/** "Fri 20 Nov 2026", from unix milliseconds. */
export function fullDate(ms: number): string {
	return `${dayDate(ms)} ${parts(ms).year}`;
}

/** "20 Nov", from unix milliseconds. */
export function dayMonth(ms: number): string {
	const { day, month } = parts(ms);
	return `${day} ${month}`;
}

/** "22 Nov 2026", from unix milliseconds. */
export function dayMonthYear(ms: number): string {
	return `${dayMonth(ms)} ${parts(ms).year}`;
}

/** "21:40", from unix milliseconds. */
export function clockTime(ms: number): string {
	return new Date(ms).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
}

const WORDS = ['none', 'one', 'two', 'three', 'four', 'five', 'six', 'seven', 'eight'];

/** "eight", for the bin's title; the figure past eight. */
export function countWord(n: number): string {
	return WORDS[n] ?? String(n);
}

/** "5th", for a place in a list. */
export function ordinal(n: number): string {
	const tens = n % 100;
	if (tens >= 11 && tens <= 13) return `${n}th`;
	return `${n}${['th', 'st', 'nd', 'rd'][n % 10] ?? 'th'}`;
}

/** "1,256", a stored count as the receipt prints it. */
export function figure(n: number): string {
	return n.toLocaleString();
}

/** "35%", a taste's share; null when the person has no plays to share out. */
export function shareOf(share: number | null | undefined): string | null {
	return share === null || share === undefined ? null : `${Math.round(share * 100)}%`;
}

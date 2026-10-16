// List helpers shared by the management profile editor's sections.

/** Adds or removes one value, keeping the list's order. */
export function toggled<T>(values: T[], value: T, checked: boolean): T[] {
	if (!checked) return values.filter((item) => item !== value);
	return values.includes(value) ? values : [...values, value];
}

/** The non-empty, trimmed lines of a one-per-line textarea. */
export function lines(value: string): string[] {
	return value
		.split('\n')
		.map((item) => item.trim())
		.filter(Boolean);
}

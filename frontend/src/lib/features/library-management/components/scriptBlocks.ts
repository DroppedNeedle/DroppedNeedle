// The blocks a script editor offers: values, ready-made pieces and, for a
// tagging script, whole lines. Each names the text it puts at the cursor
// and how to show its value on an example album from the values the
// server worked out there.

export type ScriptValues = Record<string, string>;

export interface ScriptBlock {
	label: string;
	/** The text put at the cursor. */
	code: string;
	/** Its value on the example album; null shows none. */
	sample: (values: ScriptValues) => string | null;
	look: 'value' | 'ready' | 'between' | 'line';
	/** A line block starts on a line of its own. */
	ownLine?: boolean;
	/** Where the cursor goes in `code` instead of selecting it: a line that
	 * needs a value puts the cursor inside its empty quotes. */
	caret?: number;
}

export interface ScriptBlockGroup {
	name: string;
	blocks: ScriptBlock[];
}

const pad2 = (text: string) => (/^\d+$/.test(text) ? text.padStart(2, '0') : text);
const bracketed = (text: string | undefined) => (text ? ` (${text})` : '');

function value(label: string, name: string): ScriptBlock {
	return { label, code: `{${name}}`, sample: (values) => values[name] ?? '', look: 'value' };
}

function padded(label: string, name: string): ScriptBlock {
	return {
		label,
		code: `{${name}:02d}`,
		sample: (values) => pad2(values[name] ?? ''),
		look: 'value'
	};
}

function between(label: string, code: string): ScriptBlock {
	return { label, code, sample: () => null, look: 'between' };
}

function ready(label: string, code: string, sample: (values: ScriptValues) => string): ScriptBlock {
	return { label, code, sample, look: 'ready' };
}

function inBrackets(label: string, name: string): ScriptBlock {
	return ready(
		label,
		`{conditional(is_empty(${name}), "", concat(" (", ${name}, ")"))}`,
		(values) => bracketed(values[name])
	);
}

export const NAMING_BLOCKS: ScriptBlockGroup[] = [
	{
		name: 'Ready-made',
		blocks: [
			inBrackets('Year in brackets', 'year'),
			ready(
				'Disc number, multi-disc only',
				'{conditional(equals(total_discs, "1"), "", concat(pad(disc, 2), "-"))}',
				(values) => (values.total_discs === '1' ? '' : `${pad2(values.disc ?? '')}-`)
			),
			inBrackets('Edition note in brackets', 'album_disambiguation'),
			inBrackets('Artist disambiguation in brackets', 'albumartist_disambiguation'),
			ready('Initial folder', '{initial}/', (values) => `${values.initial ?? ''}/`)
		]
	},
	{
		name: 'Names',
		blocks: [
			value('Album artist', 'albumartist'),
			value('Track artist', 'artist'),
			value('Initial', 'initial'),
			value('Artist disambiguation', 'albumartist_disambiguation')
		]
	},
	{
		name: 'Album',
		blocks: [
			value('Album', 'album'),
			value('Year', 'year'),
			value('Release type', 'release_type'),
			value('Edition note', 'album_disambiguation')
		]
	},
	{
		name: 'Track',
		blocks: [padded('Disc', 'disc'), padded('Track', 'track'), value('Title', 'title')]
	},
	{
		name: 'File',
		blocks: [value('Extension', 'ext'), value('Quality', 'quality'), value('Codec', 'codec')]
	},
	{
		name: 'Between',
		blocks: [
			between('/ new folder', '/'),
			between('space', ' '),
			between('-', ' - '),
			between('_', '_'),
			between('.', '.')
		]
	}
];

function line(label: string, code: string): ScriptBlock {
	const quotes = code.indexOf('""');
	return {
		label,
		code,
		sample: () => null,
		look: 'line',
		ownLine: true,
		caret: quotes < 0 ? undefined : quotes + 1
	};
}

function tag(label: string, name: string): ScriptBlock {
	return { label, code: name, sample: (values) => values[name] ?? '', look: 'value' };
}

export const TAGGING_BLOCKS: ScriptBlockGroup[] = [
	{
		name: 'Lines',
		blocks: [
			line('Set a tag', 'set comment = ""'),
			line('Add to a tag', 'append genre = ""'),
			line('Remove a tag', 'delete comment'),
			line('Only when...', 'if equals(total_discs, "1"):\n    delete disc_subtitle\nend')
		]
	},
	{
		name: 'Names',
		blocks: [tag('Album artist', 'album_artist'), tag('Track artist', 'artist')]
	},
	{
		name: 'Album',
		blocks: [
			tag('Album', 'album'),
			tag('Date', 'date'),
			tag('Release type', 'release_type'),
			tag('Label', 'label'),
			tag('Total discs', 'total_discs')
		]
	},
	{
		name: 'Track',
		blocks: [
			tag('Title', 'title'),
			tag('Disc number', 'disc_number'),
			tag('Track number', 'track_number'),
			tag('Comment', 'comment')
		]
	}
];

/**
 * The span of `next` that differs from `previous`: what is left once the
 * text both share at the start and at the end is taken off. Null when
 * nothing was added.
 */
export function changedSpan(previous: string, next: string): { start: number; end: number } | null {
	if (previous === next) return null;
	let start = 0;
	while (start < previous.length && start < next.length && previous[start] === next[start]) {
		start += 1;
	}
	let tail = 0;
	while (
		tail < previous.length - start &&
		tail < next.length - start &&
		previous[previous.length - 1 - tail] === next[next.length - 1 - tail]
	) {
		tail += 1;
	}
	const end = next.length - tail;
	return end > start ? { start, end } : null;
}

/** The offset of a 1-based line and column in `text`. */
export function offsetOf(text: string, line: number, column: number): number {
	let offset = 0;
	for (let at = 1; at < line; at += 1) {
		const next = text.indexOf('\n', offset);
		if (next < 0) return text.length;
		offset = next + 1;
	}
	return Math.min(offset + Math.max(column - 1, 0), text.length);
}

/**
 * `text` with the misspelt `found` on the given line replaced by `name`:
 * the one at the column when it is there, else the first on that line.
 * Null when the line does not hold it.
 */
export function replaceName(
	text: string,
	line: number,
	column: number,
	found: string,
	name: string
): string | null {
	const at = offsetOf(text, line, column);
	const lineStart = offsetOf(text, line, 1);
	const lineEnd = text.indexOf('\n', lineStart) < 0 ? text.length : text.indexOf('\n', lineStart);
	const named = (index: number) =>
		index >= lineStart && index + found.length <= lineEnd && text.startsWith(found, index);
	const candidates = [at, at + 1, text.indexOf(found, lineStart)];
	const index = candidates.find(named);
	if (index === undefined) return null;
	return text.slice(0, index) + name + text.slice(index + found.length);
}

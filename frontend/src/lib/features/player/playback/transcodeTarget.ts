export type TranscodeFormat = 'flac' | 'opus' | 'mp3';

// What the audio element is asked about each format the library admits, plus
// ALAC, which the server names apart from AAC in an MP4 file.
const PROBES: Record<string, string> = {
	flac: 'audio/flac',
	mp3: 'audio/mpeg',
	ogg: 'audio/ogg; codecs="vorbis"',
	opus: 'audio/ogg; codecs="opus"',
	m4a: 'audio/mp4; codecs="mp4a.40.2"',
	aac: 'audio/aac',
	wav: 'audio/wav',
	alac: 'audio/mp4; codecs="alac"'
};

const LOSSLESS = new Set(['alac', 'flac', 'wav']);

let probe: HTMLAudioElement | null = null;

/**
 * Whether this browser says it plays the format. Support varies by browser and
 * version, so the browser is always asked. A format it has no probe for, or no
 * format at all, is left to the load to find out.
 */
export function browserPlays(format: string | undefined): boolean {
	return plays(format?.toLowerCase() ?? '');
}

function plays(format: string): boolean {
	const type = PROBES[format];
	if (!type || typeof document === 'undefined') return true;
	probe ??= document.createElement('audio');
	return probe.canPlayType(type) !== '';
}

/**
 * The format to ask the server for instead of the original bytes, or null to
 * play the original. Lossless stays lossless, so nobody loses quality to a
 * browser's codec list. `directFailed` says the original was tried and did not
 * play: an MP4 file that fails where AAC plays is taken for ALAC, since a
 * playlist row or a media server can say `m4a` for either, and so is a file
 * whose format nobody named.
 */
export function transcodeTarget(
	sourceFormat: string | undefined,
	directFailed = false
): TranscodeFormat | null {
	const source = sourceFormat?.toLowerCase() ?? '';
	if (!directFailed && plays(source)) return null;
	const lossless = LOSSLESS.has(source) || (directFailed && (source === 'm4a' || source === ''));
	const order: TranscodeFormat[] = lossless ? ['flac', 'opus', 'mp3'] : ['opus', 'mp3'];
	return order.find((format) => format !== source && plays(format)) ?? null;
}

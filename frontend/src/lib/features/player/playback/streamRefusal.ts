import { api } from '$lib/api/client';

// How long the player waits to hear why a stream failed before it moves on.
const ASK_TIMEOUT_MS = 5_000;

/** What the player says about each refusal it names, after the track's name. */
export const REFUSAL_SAYS = {
	media_server_down: "your media server isn't answering",
	busy: 'the server is busy, try again shortly',
	missing: "the file isn't there",
	unreachable: "the server can't be reached",
	refused: "the server couldn't send it"
} as const;

/** A refusal the player names. */
export type SaidRefusal = keyof typeof REFUSAL_SAYS;

/** Why the server would not send a stream: one the player names, or a conversion it is not making. */
export type StreamRefusal = SaidRefusal | 'not_converting';

export function isSaidRefusal(value: string): value is SaidRefusal {
	return Object.hasOwn(REFUSAL_SAYS, value);
}

/**
 * Why the server refused a stream the audio element could not play, or null
 * when it sends the stream, so the browser could not read it. The element
 * reads a refusal the same as a file it cannot decode, so the same address is
 * asked again with a HEAD. That answer has no body, so its status says which:
 * the stream routes give each refusal its own (502 is a media server failing
 * the read, 409 a format asked for while the server is not converting).
 */
export async function askWhyRefused(
	url: string,
	mediaServer: boolean
): Promise<StreamRefusal | null> {
	let status: number;
	try {
		status = (await api.head(url, { timeoutMs: ASK_TIMEOUT_MS })).status;
	} catch {
		return 'unreachable';
	}
	if (status < 300) return null;
	if (status === 404) return 'missing';
	if (status === 409) return 'not_converting';
	if (status === 429 || status === 503) return 'busy';
	if (status === 502 && mediaServer) return 'media_server_down';
	return 'refused';
}

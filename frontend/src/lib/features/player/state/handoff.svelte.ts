import { createUuid } from '$lib/utils/uuid';
import { withBasePath } from '$lib/utils/basePath';
import { v3 } from '$lib/api/v3/endpoint';
import type { components } from '$lib/api/v3/openapi';
import { requestHandoff, syncHandoff } from '../queries/handoff';
import { playerStore } from './player.svelte';

type Schema = components['schemas'];
type Command = Schema['HandoffCommand'];
type Direction = 'send' | 'receive';
type Receipt = Schema['HandoffReceipt'];

function canSend() {
	return (
		(playerStore.playbackState === 'paused' || playerStore.isPlaying) &&
		playerStore.queue.length > 0 &&
		playerStore.queue.length <= 2000 &&
		playerStore.queue.every((item) => item.sourceType === 'local' && !item.isPreview)
	);
}

function snapshot(): Schema['HandoffQueue'] {
	const { queue, shuffleEnabled, shuffleOrder, currentIndex, isPlaying, progress } = playerStore;
	const ordered = shuffleEnabled ? shuffleOrder.map((index) => queue[index]) : queue;
	return {
		tracks: ordered.map((item) => ({
			id: item.trackSourceId,
			title: item.trackName,
			artist: item.artistName,
			album: item.albumName,
			album_id: item.albumId || undefined,
			artist_id: item.artistId || undefined,
			duration: item.duration
		})),
		index: shuffleEnabled ? shuffleOrder.indexOf(currentIndex) : currentIndex,
		position_ms: Math.round(progress * 1000),
		playing: isPlaying
	};
}

function receive(queue: Schema['HandoffQueue'], signal: AbortSignal) {
	const items = queue.tracks.map((track) => ({
		trackSourceId: track.id,
		trackName: track.title,
		artistName: track.artist,
		albumName: track.album,
		albumId: track.album_id ?? '',
		artistId: track.artist_id ?? undefined,
		duration: track.duration ?? undefined,
		sourceType: 'local' as const,
		trackNumber: 0,
		coverUrl: track.album_id
			? withBasePath(v3('/api/v3/library/albums/{id}/artwork', { path: { id: track.album_id } }))
			: null
	}));
	return playerStore.receiveQueue(
		items,
		queue.index,
		queue.position_ms / 1000,
		queue.playing,
		signal
	);
}

let enabled = $state(false);
let name = $state('Web player');
let players = $state<Schema['HandoffPlayer'][]>([]);
let notice = $state('');
let pending = $state(false);
let activate: ((on: boolean) => void) | undefined;
let move: ((peer: string, direction: Direction) => Promise<void>) | undefined;
let rename: ((value: string) => void) | undefined;

export const handoff = {
	get enabled() {
		return enabled;
	},
	get available() {
		return canSend();
	},
	get players() {
		return players;
	},
	get name() {
		return name;
	},
	get notice() {
		return notice;
	},
	get pending() {
		return pending;
	},
	setEnabled(on: boolean) {
		activate?.(on);
	},
	setName(value: string) {
		rename?.(value);
	},
	move(peer: string, direction: Direction) {
		return move?.(peer, direction);
	}
};

export function connectHandoff(userId: string) {
	const key = 'dn.handoff.' + JSON.stringify([withBasePath('/'), userId]);
	let stop: (() => void) | undefined;
	function save() {
		try {
			localStorage.setItem(key, JSON.stringify({ enabled, name }));
		} catch {
			/* Tab-only in private browsing. */
		}
	}
	name = 'Web player';
	let remembered = false;
	try {
		const saved = JSON.parse(localStorage.getItem(key) ?? 'null');
		remembered = saved?.enabled === true;
		if (typeof saved?.name === 'string' && saved.name.trim()) name = saved.name.trim().slice(0, 60);
	} catch {
		/* Use the defaults if storage is unavailable. */
	}
	function enable(on: boolean) {
		stop?.();
		stop = undefined;
		enabled = on;
		players = [];
		pending = false;
		notice = '';
		move = undefined;
		if (!on) return;
		stop = startPlayer();
	}
	activate = (on) => {
		enable(on);
		save();
	};
	rename = (value) => {
		name =
			Array.from(value.trim())
				.filter((c) => c >= ' ' && !(c >= '\u007f' && c <= '\u009f'))
				.slice(0, 60)
				.join('') || 'Web player';
		save();
	};
	enable(remembered);
	return () => {
		enable(false);
		activate = undefined;
		rename = undefined;
	};
}

function startPlayer() {
	const id = createUuid();
	const life = new AbortController();
	let timer: ReturnType<typeof setTimeout>;
	let last: { kind: Command['kind']; receipt: Receipt } | undefined;
	let receipt: Receipt | undefined;
	const disconnected = 'Could not reach the linked players. Retrying…';
	let offered: { transfer: string; play: number; queue: Schema['HandoffQueue'] } | undefined;
	let received: { transfer: string; play: number } | undefined;
	let requested: { id: string; until: number } | undefined;

	function cancelUnconfirmed() {
		if (received && received.play === playerStore.playId && playerStore.isPlaying)
			playerStore.pause();
		received = undefined;
	}

	async function handle(command: Command): Promise<Receipt | undefined> {
		const transfer_id = command.transfer_id;
		if (command.kind === 'failed') {
			if (received?.transfer === transfer_id) cancelUnconfirmed();
			if (!requested || requested.id === transfer_id) {
				notice = 'The handoff did not finish.';
				pending = false;
				requested = undefined;
			}
			offered = undefined;
			return;
		}
		if (last?.receipt.transfer_id === transfer_id && last.kind === command.kind)
			return last.receipt;
		if (command.kind === 'offer' && canSend()) {
			const queue = snapshot();
			// Reject here so a too-large offer cannot get stuck in the heartbeat retry loop.
			if (new TextEncoder().encode(JSON.stringify(queue)).length <= 512 * 1024) {
				offered = { transfer: transfer_id, play: playerStore.playId, queue };
				return { kind: 'offered', transfer_id, queue };
			}
		}
		if (command.kind === 'receive') {
			const signal = AbortSignal.any([life.signal, AbortSignal.timeout(12_000)]);
			// A tab can close or a browser can block audio while the media element loads.
			const accepted = await new Promise<boolean>((resolve) => {
				const aborted = () => resolve(false);
				signal.addEventListener('abort', aborted, { once: true });
				void receive(command.queue, signal)
					.then(resolve, aborted)
					.finally(() => signal.removeEventListener('abort', aborted));
			});
			if (!life.signal.aborted) {
				if (accepted && command.queue.playing)
					received = { transfer: transfer_id, play: playerStore.playId };
				notice = accepted
					? 'Queue moved to ' + name + '.'
					: 'This browser could not take over playback.';
				requested = undefined;
				pending = false;
			}
			return { kind: accepted ? 'accepted' : 'declined', transfer_id };
		}
		if (command.kind === 'release') {
			if (
				offered?.transfer === transfer_id &&
				offered.play === playerStore.playId &&
				offered.queue.playing
			) {
				const current = snapshot();
				const previous = offered.queue;
				if (
					current.index === previous.index &&
					(current.playing || playerStore.isBuffering) &&
					current.tracks.length === previous.tracks.length &&
					current.tracks.every((track, i) => track.id === previous.tracks[i].id)
				)
					playerStore.pause();
			}
			const destination = players.find((peer) => peer.id === command.destination_id);
			notice = 'Queue moved to ' + (destination?.name ?? 'another player') + '.';
			pending = false;
			requested = undefined;
			offered = undefined;
			return { kind: 'released', transfer_id };
		}
		notice = 'This queue could not be handed off.';
		return { kind: 'declined', transfer_id };
	}
	async function sync() {
		let delay = 2000;
		try {
			const update = await syncHandoff(
				{ player_id: id, name, can_send: canSend(), receipt },
				AbortSignal.any([life.signal, AbortSignal.timeout(6000)])
			);
			if (life.signal.aborted) return;
			if (notice === disconnected) notice = '';
			players = update.players.filter((peer) => peer.id !== id);
			// Once the server confirms acceptance, this tab owns playback even if the source closes.
			if (receipt?.kind === 'accepted' && update.command?.kind !== 'failed') received = undefined;
			receipt = undefined;
			if (update.command) {
				const reply = await handle(update.command);
				if (life.signal.aborted) return;
				if (reply) {
					last = { kind: update.command.kind, receipt: reply };
					receipt = reply;
					delay = 100;
				}
			}
		} catch {
			if (!life.signal.aborted) {
				players = [];
				notice = disconnected;
				delay = 5000;
			}
		} finally {
			if (!life.signal.aborted) {
				if (requested && Date.now() > requested.until) {
					pending = false;
					requested = undefined;
					notice = 'The handoff did not finish.';
				}
				timer = setTimeout(() => void sync(), delay);
			}
		}
	}
	move = async (peer, direction) => {
		if (pending) return;
		pending = true;
		const transfer_id = createUuid();
		requested = { id: transfer_id, until: Date.now() + 95_000 };
		notice = 'Moving the queue…';
		try {
			await requestHandoff(
				{
					transfer_id,
					player_id: id,
					source_id: direction === 'receive' ? peer : id,
					destination_id: direction === 'receive' ? id : peer
				},
				AbortSignal.any([life.signal, AbortSignal.timeout(6000)])
			);
		} catch {
			if (!life.signal.aborted && requested?.id === transfer_id) {
				pending = false;
				requested = undefined;
				notice = 'Could not start the handoff.';
			}
		}
	};
	void sync();
	return () => {
		life.abort();
		cancelUnconfirmed();
		clearTimeout(timer);
		void syncHandoff(
			{ player_id: id, name, can_send: false, disconnect: true },
			AbortSignal.timeout(3000)
		).catch(() => {});
	};
}

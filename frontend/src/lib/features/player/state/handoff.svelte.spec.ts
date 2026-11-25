import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { connectHandoff, handoff } from './handoff.svelte';
import { requestHandoff, syncHandoff } from '../queries/handoff';
import { playerStore } from './player.svelte';
import type { components } from '$lib/api/v3/openapi';

vi.mock('../queries/handoff', () => ({ syncHandoff: vi.fn(), requestHandoff: vi.fn() }));
vi.mock('./player.svelte', () => ({
	playerStore: {
		playbackState: 'playing',
		isPlaying: true,
		playId: 1,
		progress: 2.5,
		currentIndex: 1,
		queue: ['file1', 'file1'].map((trackSourceId) => ({
			trackSourceId,
			trackName: 'Title',
			artistName: 'Artist',
			albumName: 'Album',
			albumId: '',
			sourceType: 'local'
		})),
		shuffleEnabled: false,
		shuffleOrder: [],
		pause: vi.fn(),
		receiveQueue: vi.fn()
	}
}));

const peer = { id: 'other-player', name: 'Kitchen', can_send: true };
const update = (command?: components['schemas']['HandoffCommand']) => ({
	players: [peer],
	command
});
const transfer_id = 'transfer';
let close: () => void;
beforeEach(() => {
	vi.useFakeTimers();
	vi.resetAllMocks();
	localStorage.clear();
	vi.mocked(syncHandoff).mockResolvedValue(update());
	close = connectHandoff('listener');
});
afterEach(() => {
	close();
	vi.useRealTimers();
});

describe('web player handoff', () => {
	it('opts in per account, sends a duplicate track occurrence, and pauses only on confirmed release', async () => {
		expect(syncHandoff).not.toHaveBeenCalled();
		handoff.setEnabled(true);
		await vi.advanceTimersByTimeAsync(0);
		await handoff.move(peer.id, 'send');
		expect(requestHandoff).toHaveBeenCalledWith(
			expect.objectContaining({ source_id: expect.any(String), destination_id: peer.id }),
			expect.any(AbortSignal)
		);
		vi.mocked(syncHandoff).mockResolvedValueOnce(update({ kind: 'offer', transfer_id }));
		await vi.advanceTimersByTimeAsync(2100);
		expect(syncHandoff).toHaveBeenLastCalledWith(
			expect.objectContaining({
				receipt: expect.objectContaining({
					kind: 'offered',
					queue: expect.objectContaining({
						index: 1,
						position_ms: 2500,
						playing: true,
						tracks: [
							expect.objectContaining({ id: 'file1' }),
							expect.objectContaining({ id: 'file1' })
						]
					})
				})
			}),
			expect.any(AbortSignal)
		);
		expect(playerStore.pause).not.toHaveBeenCalled();
		vi.mocked(syncHandoff).mockResolvedValueOnce(
			update({ kind: 'release', transfer_id, destination_id: peer.id })
		);
		await vi.advanceTimersByTimeAsync(2000);
		expect(playerStore.pause).toHaveBeenCalledTimes(1);
		expect(handoff.notice).toContain('Kitchen');
		close();
		expect(syncHandoff).toHaveBeenLastCalledWith(
			expect.objectContaining({ disconnect: true }),
			expect.any(AbortSignal)
		);
		close = connectHandoff('someone-else');
		expect(handoff.enabled).toBe(false);
		expect(handoff.players).toEqual([]);
	});
	it('withdraws playback when the tab opts out before its acceptance reaches the server', async () => {
		vi.mocked(syncHandoff).mockResolvedValueOnce(
			update({
				kind: 'receive',
				transfer_id,
				queue: {
					index: 0,
					position_ms: 0,
					playing: true,
					tracks: [{ id: 'f', title: 'Title', artist: 'Artist', album: 'Album' }]
				}
			})
		);
		vi.mocked(playerStore.receiveQueue).mockResolvedValue(true);
		handoff.setEnabled(true);
		await vi.advanceTimersByTimeAsync(0);
		handoff.setEnabled(false);
		expect(playerStore.pause).toHaveBeenCalledOnce();
		expect(syncHandoff).toHaveBeenCalledTimes(2);
	});

	it.each([true, false])(
		'receives once and reports the actual restore result (%s) after a repeated command',
		async (accepted) => {
			const command = {
				kind: 'receive' as const,
				transfer_id,
				queue: {
					index: 0,
					position_ms: 7800,
					playing: false,
					tracks: [{ id: 'file1', title: 'Title', artist: 'Artist', album: 'Album' }]
				}
			};
			vi.mocked(syncHandoff)
				.mockResolvedValueOnce(update(command))
				.mockResolvedValueOnce(update(command));
			vi.mocked(playerStore.receiveQueue).mockResolvedValue(accepted);
			handoff.setEnabled(true);
			await vi.advanceTimersByTimeAsync(210);
			expect(playerStore.receiveQueue).toHaveBeenCalledTimes(1);
			expect(playerStore.receiveQueue).toHaveBeenCalledWith(
				[expect.objectContaining({ trackSourceId: 'file1', sourceType: 'local' })],
				0,
				7.8,
				false,
				expect.any(AbortSignal)
			);
			expect(syncHandoff).toHaveBeenLastCalledWith(
				expect.objectContaining({
					receipt: { transfer_id, kind: accepted ? 'accepted' : 'declined' }
				}),
				expect.any(AbortSignal)
			);
		}
	);
});

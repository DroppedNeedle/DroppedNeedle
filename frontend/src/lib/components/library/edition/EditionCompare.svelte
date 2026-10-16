<!--
	The edition's tracks beside the person's files, in the edition's order.
	A track a file is another version of is one row with both lengths; files
	the edition has no place for follow. Rows stack on a phone.
	On the album's own edition a curator can match a file the edition did
	not place to a track no file holds, and undo a match made that way until
	DroppedNeedle writes the file.
-->
<script lang="ts">
	import { tick } from 'svelte';
	import { Check, Undo2 } from 'lucide-svelte';
	import { ApiError } from '$lib/api/client';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import Button from '$lib/ui/Button.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import type { EditionPlan } from '$lib/queries/albums/EditionQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { formatDuration } from '$lib/utils/formatting';
	import {
		ITS_THIS_TRACK,
		MATCH,
		MATCHED_BY_HAND,
		MATCH_FAILED,
		MATCH_MAKES_IT_YOURS,
		MATCH_PICKER_LABEL,
		MATCH_STALE,
		MATCH_TO_TRACK,
		MATCH_WRITTEN,
		MATCH_WRITTEN_PARTS,
		UNDO_MATCH,
		UNMATCH_FAILED,
		failureSentence,
		forFile,
		freeTrackLabel
	} from './editionCopy';

	interface Props {
		plan: EditionPlan;
		/** A curator on the album's own MusicBrainz edition: files can be matched by hand. */
		canMatch?: boolean;
		/** The app picked the edition, so a match makes it the person's. */
		appPicked?: boolean;
		/** Rejects with the server's refusal, shown on the row. */
		onmatch?: (
			trackId: string,
			releaseTrackMbid: string,
			identityRevision: number
		) => Promise<void>;
		onunmatch?: (trackId: string) => Promise<void>;
	}

	let { plan, canMatch = false, appPicked = false, onmatch, onunmatch }: Props = $props();

	const lineId = $props.id();

	type RowState = 'fits' | 'missing' | 'version' | 'extra' | 'second';
	interface Side {
		title: string;
		length: number | null | undefined;
	}
	interface CompareRow {
		key: string;
		at: string;
		edition: Side | null;
		file: Side | null;
		state: RowState;
		/** The file on the row, when there is one. */
		fileId: string | null;
		/** The edition track on the row, when there is one. */
		releaseTrack: string | null;
		byHand: boolean;
		/** DroppedNeedle wrote the file since it was matched by hand. */
		written: boolean;
	}

	const CHIPS: Record<Exclude<RowState, 'fits' | 'missing'>, string> = {
		version: 'Another version',
		extra: 'Not on this edition',
		second: 'Second copy'
	};

	const spot = (disc: number | null | undefined, position: number | null | undefined) =>
		`${disc}.${position}`;

	const rows = $derived.by((): CompareRow[] => {
		const files = new Map(plan.files.map((file) => [file.local_track_id, file]));
		const versionOf = new Map(
			plan.files
				.filter((file) => file.fate === 'other_version')
				.map((file) => [spot(file.edition_disc_number, file.edition_position), file])
		);
		const out: CompareRow[] = plan.tracks.map((track) => {
			const at = spot(track.disc_number, track.position);
			const edition = { title: track.title, length: track.length_ms };
			const row = {
				key: track.release_track_mbid,
				at,
				edition,
				releaseTrack: track.release_track_mbid
			};
			const held = track.local_track_id ? files.get(track.local_track_id) : undefined;
			if (held) {
				const file = { title: held.title, length: held.length_ms };
				return {
					...row,
					file,
					state: 'fits',
					fileId: held.local_track_id,
					byHand: held.matched_by_hand,
					written: held.match_written
				};
			}
			const version = versionOf.get(at);
			if (version) {
				const file = { title: version.title, length: version.length_ms };
				return {
					...row,
					file,
					state: 'version',
					fileId: version.local_track_id,
					byHand: false,
					written: false
				};
			}
			return { ...row, file: null, state: 'missing', fileId: null, byHand: false, written: false };
		});
		for (const file of plan.files) {
			if (file.fate !== 'extra' && file.fate !== 'duplicate') continue;
			out.push({
				key: file.local_track_id,
				at: '',
				edition: null,
				file: { title: file.title, length: file.length_ms },
				state: file.fate === 'extra' ? 'extra' : 'second',
				fileId: file.local_track_id,
				releaseTrack: null,
				byHand: false,
				written: false
			});
		}
		return out;
	});

	const freeTracks = $derived(plan.tracks.filter((track) => !track.local_track_id));

	// Free tracks a file is already another version of: a likely home for
	// that file, not for the one being matched.
	const versionSpots = $derived(
		new Set(
			plan.files
				.filter((file) => file.fate === 'other_version')
				.map((file) => spot(file.edition_disc_number, file.edition_position))
		)
	);

	/**
	 * The free track the picker starts on: the closest in length among those
	 * no other version sits beside, else the first free one.
	 */
	function suggest(row: CompareRow): string {
		const open = freeTracks.filter(
			(track) => !versionSpots.has(spot(track.disc_number, track.position))
		);
		const length = row.file?.length;
		if (!open.length || !length) return (open[0] ?? freeTracks[0])?.release_track_mbid ?? '';
		const gap = (ms: number | null | undefined) => (ms ? Math.abs(ms - length) : Infinity);
		let best = open[0];
		for (const track of open) if (gap(track.length_ms) < gap(best.length_ms)) best = track;
		return best.release_track_mbid;
	}

	let picking = $state<string | null>(null);
	let pick = $state('');
	let errors = $state<Record<string, string>>({});
	let select = $state<HTMLSelectElement | null>(null);
	// Per row: its match button, its undo button, its chip and the row itself.
	let entries = $state<Record<string, HTMLElement | null>>({});
	let undos = $state<Record<string, HTMLElement | null>>({});
	let chips = $state<Record<string, HTMLElement | null>>({});
	let rowEls = $state<Record<string, HTMLElement | null>>({});
	// Where focus goes once the plan read after a match or an undo shows it.
	let focusAfter = $state<{ fileId: string; placed: boolean } | null>(null);

	// Another edition starts clean. A plan read again for the same edition
	// keeps the open picker and each row's message.
	const release = $derived(plan.release_mbid);
	$effect(() => {
		void release;
		picking = null;
		errors = {};
		focusAfter = null;
	});

	// A plan read again may have taken the picked track, or every track.
	$effect(() => {
		if (picking === null) return;
		if (!freeTracks.length) {
			picking = null;
			return;
		}
		if (freeTracks.some((track) => track.release_track_mbid === pick)) return;
		const row = rows.find((candidate) => candidate.key === picking);
		pick = row ? suggest(row) : (freeTracks[0]?.release_track_mbid ?? '');
	});

	$effect(() => {
		const target = focusAfter;
		if (!target) return;
		const row = rows.find(
			(candidate) =>
				candidate.fileId === target.fileId && (candidate.state === 'fits') === target.placed
		);
		if (!row) return;
		const element = target.placed
			? (undos[row.key] ?? chips[row.key])
			: (entries[row.key] ?? rowEls[row.key]);
		if (!element) return;
		focusAfter = null;
		focusOn(element);
	});

	// Brings the whole row into view, clear of the sheet's footer on a phone.
	function focusOn(element: HTMLElement | null | undefined): void {
		element?.focus({ preventScroll: true });
		element?.closest('[role="row"]')?.scrollIntoView({ block: 'nearest' });
	}

	async function openPicker(row: CompareRow): Promise<void> {
		delete errors[row.key];
		pick = suggest(row);
		picking = row.key;
		await tick();
		focusOn(select);
	}

	async function cancel(row: CompareRow): Promise<void> {
		picking = null;
		await tick();
		focusOn(entries[row.key]);
	}

	async function match(row: CompareRow, releaseTrack: string): Promise<void> {
		if (!onmatch || !row.fileId || !releaseTrack) return;
		delete errors[row.key];
		try {
			await onmatch(row.fileId, releaseTrack, plan.identity_revision);
			picking = null;
			focusAfter = { fileId: row.fileId, placed: true };
		} catch (error) {
			errors[row.key] =
				error instanceof ApiError && error.code === 'STALE_REVISION'
					? MATCH_STALE
					: failureSentence(error, MATCH_FAILED);
		}
	}

	async function unmatch(row: CompareRow): Promise<void> {
		if (!onunmatch || !row.fileId) return;
		delete errors[row.key];
		try {
			await onunmatch(row.fileId);
			focusAfter = { fileId: row.fileId, placed: false };
		} catch (error) {
			errors[row.key] = failureSentence(error, UNMATCH_FAILED);
		}
	}

	function hasActions(row: CompareRow): boolean {
		if (!canMatch) return false;
		if (row.state === 'fits') return row.byHand;
		if (row.state === 'version') return true;
		if (row.state === 'extra' || row.state === 'second') return freeTracks.length > 0;
		return false;
	}

	// Said once, above the table, while some row offers a match.
	const showLine = $derived(
		appPicked && rows.some((row) => row.state !== 'fits' && hasActions(row))
	);
	const describedBy = $derived(showLine ? lineId : undefined);
</script>

{#if showLine}
	<p id={lineId} class="text-xs text-base-content/65">{MATCH_MAKES_IT_YOURS}</p>
{/if}
<div
	class="rounded-box border border-base-content/10"
	role="table"
	aria-label="This edition and your files"
>
	<div
		class="hidden grid-cols-[3rem_minmax(0,1fr)_minmax(0,1fr)_8.5rem] gap-3 border-b border-base-content/10 px-3 py-2 text-xs font-semibold text-base-content/55 md:grid"
		role="row"
	>
		<span role="columnheader">#</span>
		<span role="columnheader">On this edition</span>
		<span role="columnheader">Your file</span>
		<span role="columnheader"><span class="sr-only">How it fits</span></span>
	</div>
	{#each rows as row (row.key)}
		{@const title = row.file?.title ?? ''}
		<div
			bind:this={rowEls[row.key]}
			tabindex="-1"
			class="grid scroll-mb-20 grid-cols-[2.5rem_minmax(0,1fr)] gap-x-3 gap-y-0.5 border-b border-base-content/5 px-3 py-2 text-sm last:border-b-0 md:grid-cols-[3rem_minmax(0,1fr)_minmax(0,1fr)_8.5rem] md:items-center {row.state ===
			'missing'
				? 'bg-warning/10'
				: ''}"
			role="row"
		>
			<span class="text-base-content/50 tabular-nums" role="cell">{row.at}</span>
			<span class="min-w-0" role="cell">
				{#if row.edition}
					<span class="break-words">{row.edition.title}</span>
					<span class="ml-1 text-base-content/55 tabular-nums"
						>{formatDuration(row.edition.length)}</span
					>
				{/if}
			</span>
			<span class="col-start-2 min-w-0 md:col-start-auto" role="cell">
				{#if row.file}
					<span class="text-base-content/55 md:hidden">Your file:</span>
					<span class="break-words">{row.file.title}</span>
					<span class="ml-1 text-base-content/55 tabular-nums"
						>{formatDuration(row.file.length)}</span
					>
				{:else}
					<span class="font-medium">Missing</span>
				{/if}
			</span>
			<span class="col-start-2 flex flex-wrap items-center gap-1 md:col-start-auto" role="cell">
				{#if row.state === 'fits'}
					<Check class="h-4 w-4 text-success" aria-hidden="true" /><span class="sr-only">Fits</span>
					{#if canMatch && row.byHand}
						<span class="badge badge-sm badge-ghost" tabindex="-1" bind:this={chips[row.key]}
							>{MATCHED_BY_HAND}</span
						>
					{/if}
				{:else if row.state !== 'missing'}
					<span class="badge badge-sm {row.state === 'version' ? 'badge-warning' : 'badge-ghost'}"
						>{CHIPS[row.state]}</span
					>
				{/if}
			</span>
			{#if hasActions(row) || errors[row.key]}
				<div class="col-start-2 flex flex-col gap-2 pt-1 md:col-end-5" role="cell">
					{#if hasActions(row)}
						{#if row.state === 'fits' && row.written}
							<p class="text-xs text-base-content/65">
								{#if authStore.isAdmin}{MATCH_WRITTEN_PARTS[0]}<a
										class="link"
										href={withBasePath('/library/management/history')}>{MATCH_WRITTEN_PARTS[1]}</a
									>{MATCH_WRITTEN_PARTS[2]}{:else}{MATCH_WRITTEN}{/if}
							</p>
						{:else if row.state === 'fits'}
							<AsyncButton
								variant="ghost"
								size="xs"
								icon={Undo2}
								class="self-start"
								aria-label={forFile(UNDO_MATCH, title)}
								bind:ref={() => undos[row.key] ?? null, (el) => (undos[row.key] = el)}
								onclick={() => unmatch(row)}>{UNDO_MATCH}</AsyncButton
							>
						{:else if row.state === 'version'}
							<AsyncButton
								variant="outline"
								size="xs"
								class="self-start"
								aria-label={forFile(ITS_THIS_TRACK, title)}
								aria-describedby={describedBy}
								bind:ref={() => entries[row.key] ?? null, (el) => (entries[row.key] = el)}
								onclick={() => match(row, row.releaseTrack ?? '')}>{ITS_THIS_TRACK}</AsyncButton
							>
						{:else if picking === row.key}
							<div class="flex flex-col gap-2 md:flex-row md:items-end">
								<div class="w-full md:max-w-sm">
									<SelectField
										label={MATCH_PICKER_LABEL}
										bind:value={pick}
										bind:ref={select}
										options={freeTracks.map((track) => ({
											value: track.release_track_mbid,
											label: freeTrackLabel(track)
										}))}
									/>
								</div>
								<div class="flex gap-2 md:pb-1">
									<AsyncButton
										variant="primary"
										size="sm"
										aria-label={forFile(MATCH, title)}
										aria-describedby={describedBy}
										onclick={() => match(row, pick)}>{MATCH}</AsyncButton
									>
									<Button variant="ghost" size="sm" onclick={() => cancel(row)}>Cancel</Button>
								</div>
							</div>
						{:else}
							<Button
								variant="outline"
								size="xs"
								class="self-start"
								aria-label={forFile(MATCH_TO_TRACK, title)}
								aria-describedby={describedBy}
								bind:ref={() => entries[row.key] ?? null, (el) => (entries[row.key] = el)}
								onclick={() => openPicker(row)}>{MATCH_TO_TRACK}</Button
							>
						{/if}
					{/if}
					{#if errors[row.key]}
						<p class="text-sm text-error" role="alert">{errors[row.key]}</p>
					{/if}
				</div>
			{/if}
		</div>
	{/each}
</div>

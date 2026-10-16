<!--
	The edition's tracks beside the person's files, in the edition's order.
	A track a file is another version of is one row with both lengths; files
	the edition has no place for follow. Rows stack on a phone.
	On the album's own edition a curator can match a file the edition did
	not place to a track no file holds, and undo a match made that way.
-->
<script lang="ts">
	import { Check, Undo2 } from 'lucide-svelte';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import Button from '$lib/ui/Button.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import type { EditionPlan } from '$lib/queries/albums/EditionQueries.svelte';
	import { formatDuration } from '$lib/utils/formatting';
	import {
		ITS_THIS_TRACK,
		MATCH,
		MATCHED_BY_YOU,
		MATCH_FAILED,
		MATCH_MAKES_IT_YOURS,
		MATCH_PICKER_LABEL,
		MATCH_TO_TRACK,
		UNDO_MATCH,
		UNMATCH_FAILED,
		failureSentence,
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
	}

	const CHIPS: Record<Exclude<RowState, 'fits' | 'missing'>, string> = {
		version: 'Another version',
		extra: 'Not on this edition',
		second: 'Second copy'
	};

	const rows = $derived.by((): CompareRow[] => {
		const files = new Map(plan.files.map((file) => [file.local_track_id, file]));
		const versionOf = new Map(
			plan.files
				.filter((file) => file.fate === 'other_version')
				.map((file) => [`${file.edition_disc_number}.${file.edition_position}`, file])
		);
		const out: CompareRow[] = plan.tracks.map((track) => {
			const at = `${track.disc_number}.${track.position}`;
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
					byHand: held.matched_by_hand
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
					byHand: false
				};
			}
			return { ...row, file: null, state: 'missing', fileId: null, byHand: false };
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
				byHand: false
			});
		}
		return out;
	});

	const freeTracks = $derived(plan.tracks.filter((track) => !track.local_track_id));

	let picking = $state<string | null>(null);
	let pick = $state('');
	let errors = $state<Record<string, string>>({});

	$effect(() => {
		void plan.release_mbid;
		picking = null;
		errors = {};
	});

	function openPicker(row: CompareRow): void {
		delete errors[row.key];
		pick = freeTracks[0]?.release_track_mbid ?? '';
		picking = row.key;
	}

	async function match(row: CompareRow, releaseTrack: string): Promise<void> {
		if (!onmatch || !row.fileId || !releaseTrack) return;
		delete errors[row.key];
		try {
			await onmatch(row.fileId, releaseTrack, plan.identity_revision);
			picking = null;
		} catch (error) {
			errors[row.key] = failureSentence(error, MATCH_FAILED);
		}
	}

	async function unmatch(row: CompareRow): Promise<void> {
		if (!onunmatch || !row.fileId) return;
		delete errors[row.key];
		try {
			await onunmatch(row.fileId);
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
</script>

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
		<div
			class="grid grid-cols-[2.5rem_minmax(0,1fr)] gap-x-3 gap-y-0.5 border-b border-base-content/5 px-3 py-2 text-sm last:border-b-0 md:grid-cols-[3rem_minmax(0,1fr)_minmax(0,1fr)_8.5rem] md:items-center {row.state ===
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
						<span class="badge badge-sm badge-ghost">{MATCHED_BY_YOU}</span>
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
						{#if row.state === 'fits'}
							<AsyncButton
								variant="ghost"
								size="xs"
								icon={Undo2}
								class="self-start"
								onclick={() => unmatch(row)}>{UNDO_MATCH}</AsyncButton
							>
						{:else if row.state === 'version'}
							<AsyncButton
								variant="outline"
								size="xs"
								class="self-start"
								onclick={() => match(row, row.releaseTrack ?? '')}>{ITS_THIS_TRACK}</AsyncButton
							>
							{#if appPicked}
								<p class="text-xs text-base-content/65">{MATCH_MAKES_IT_YOURS}</p>
							{/if}
						{:else if picking === row.key}
							<div class="flex flex-col gap-2 md:flex-row md:items-end">
								<div class="w-full md:max-w-sm">
									<SelectField
										label={MATCH_PICKER_LABEL}
										bind:value={pick}
										options={freeTracks.map((track) => ({
											value: track.release_track_mbid,
											label: freeTrackLabel(track)
										}))}
									/>
								</div>
								<div class="flex gap-2 md:pb-1">
									<AsyncButton variant="primary" size="sm" onclick={() => match(row, pick)}
										>{MATCH}</AsyncButton
									>
									<Button variant="ghost" size="sm" onclick={() => (picking = null)}>Cancel</Button>
								</div>
							</div>
							{#if appPicked}
								<p class="text-xs text-base-content/65">{MATCH_MAKES_IT_YOURS}</p>
							{/if}
						{:else}
							<Button variant="outline" size="xs" class="self-start" onclick={() => openPicker(row)}
								>{MATCH_TO_TRACK}</Button
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

<!--
	The edition's tracks beside the person's files, in the edition's order.
	A track a file is another version of is one row with both lengths; files
	the edition has no place for follow. Rows stack on a phone.
-->
<script lang="ts">
	import { Check } from 'lucide-svelte';
	import type { EditionPlan } from '$lib/queries/albums/EditionQueries.svelte';
	import { formatDuration } from '$lib/utils/formatting';

	interface Props {
		plan: EditionPlan;
	}

	let { plan }: Props = $props();

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
			const held = track.local_track_id ? files.get(track.local_track_id) : undefined;
			if (held) {
				const file = { title: held.title, length: held.length_ms };
				return { key: track.release_track_mbid, at, edition, file, state: 'fits' };
			}
			const version = versionOf.get(at);
			if (version) {
				const file = { title: version.title, length: version.length_ms };
				return { key: track.release_track_mbid, at, edition, file, state: 'version' };
			}
			return { key: track.release_track_mbid, at, edition, file: null, state: 'missing' };
		});
		for (const file of plan.files) {
			if (file.fate !== 'extra' && file.fate !== 'duplicate') continue;
			out.push({
				key: file.local_track_id,
				at: '',
				edition: null,
				file: { title: file.title, length: file.length_ms },
				state: file.fate === 'extra' ? 'extra' : 'second'
			});
		}
		return out;
	});
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
			<span class="col-start-2 md:col-start-auto" role="cell">
				{#if row.state === 'fits'}
					<Check class="h-4 w-4 text-success" aria-hidden="true" /><span class="sr-only">Fits</span>
				{:else if row.state !== 'missing'}
					<span class="badge badge-sm {row.state === 'version' ? 'badge-warning' : 'badge-ghost'}"
						>{CHIPS[row.state]}</span
					>
				{/if}
			</span>
		</div>
	{/each}
</div>

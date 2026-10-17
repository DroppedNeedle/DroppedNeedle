<!--
	Preservation and format safety: scrub, preservation, validation, format
	compatibility, edition acceptance and post-write notifications.
-->
<script lang="ts">
	import '$lib/components/library/library-management.css';
	import { Check, ShieldCheck } from 'lucide-svelte';

	import LibraryManagementEditorAdvanced from './LibraryManagementEditorAdvanced.svelte';
	import LibraryManagementEditorSection from './LibraryManagementEditorSection.svelte';
	import { lines } from './managementProfileLists';
	import type {
		FileBehaviorSettings,
		FormatCompatibilitySettings,
		LibraryManagementProfile,
		MetadataManagementSettings
	} from '$lib/queries/library-management/types';
	import Field from '$lib/ui/Field.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	interface Props {
		metadata: MetadataManagementSettings;
		fileBehavior: FileBehaviorSettings;
		identity: LibraryManagementProfile['identity'];
		notification: LibraryManagementProfile['notification'];
	}

	let {
		metadata = $bindable(),
		fileBehavior = $bindable(),
		identity = $bindable(),
		notification = $bindable()
	}: Props = $props();

	const formats = $derived(metadata.format_compatibility);
	const safeguardCount = $derived(
		[
			fileBehavior.preserve_timestamps,
			fileBehavior.preserve_permissions,
			fileBehavior.strict_capability_gate,
			fileBehavior.reject_symlinks,
			fileBehavior.validate_written_metadata,
			fileBehavior.validate_technical_audio
		].filter(Boolean).length
	);

	function setId3Version(version: FormatCompatibilitySettings['id3_version']): void {
		formats.id3_version = version;
		if (version === '2.3') formats.id3_text_encoding = 'utf16';
	}
</script>

<LibraryManagementEditorSection
	icon={ShieldCheck}
	title="Preservation and format safety"
	subtitle="Compatibility, scrub, validation, and notifications"
	active={true}
	state="Protected"
	chips={[
		{ label: `${safeguardCount} safeguards`, active: safeguardCount > 0 },
		{ label: `ID3v${formats.id3_version}` },
		...(notification.refresh_external_servers ? [{ label: 'Media refresh on', active: true }] : [])
	]}
>
	<div class="mt-4 grid gap-3 sm:grid-cols-2">
		<ToggleRow
			label="Scrub unmanaged tags"
			description="Explicitly remove tags outside the allowlist."
			bind:checked={metadata.scrub_unmanaged_tags}
		/>
		<ToggleRow
			label="Preserve embedded art"
			description="Keep pictures during an explicit scrub."
			bind:checked={metadata.preserve_embedded_art_during_scrub}
		/>
		<ToggleRow
			label="Preserve timestamps"
			description="Restore source times after publish."
			bind:checked={fileBehavior.preserve_timestamps}
		/>
		<ToggleRow
			label="Read-back validation"
			description="Reject a staged file that does not match."
			bind:checked={fileBehavior.validate_written_metadata}
		/>
		<LibraryManagementEditorAdvanced
			class="sm:col-span-2"
			title="Format compatibility"
			subtitle="ID3, constrained formats, and preserved fields"
		>
			<div class="mt-3 grid gap-3 sm:grid-cols-2">
				<SelectField
					label="ID3 version"
					options={[
						{ value: '2.4', label: 'ID3v2.4' },
						{ value: '2.3', label: 'ID3v2.3' }
					]}
					bind:value={() => formats.id3_version, setId3Version}
				/>
				{#if formats.id3_version === '2.3'}
					<TextField label="ID3v2.3 list delimiter" bind:value={formats.id3v23_join_delimiter} />
				{/if}
				<SelectField
					label="ID3 text encoding"
					options={[
						...(formats.id3_version === '2.4' ? [{ value: 'utf8' as const, label: 'UTF-8' }] : []),
						{ value: 'utf16' as const, label: 'UTF-16' }
					]}
					bind:value={formats.id3_text_encoding}
				/>
				<SelectField
					label="MP3 APEv2 tags"
					help="Remove deletes the complete MP3 APEv2 tag container."
					options={[
						{ value: 'preserve', label: 'Preserve' },
						{ value: 'remove', label: 'Remove' }
					]}
					bind:value={formats.mp3_apev2_policy}
				/>
				<SelectField
					label="Raw AAC tags"
					help="Do not write preserves raw AAC bytes; Remove APEv2 also removes artwork stored there."
					options={[
						{ value: 'save_apev2', label: 'Save APEv2' },
						{ value: 'do_not_write', label: 'Do not write' },
						{ value: 'remove_apev2', label: 'Remove APEv2' }
					]}
					bind:value={formats.raw_aac_tag_policy}
				/>
				<SelectField
					label="WAV tags"
					help="Choosing ID3 or RIFF INFO may convert the active WAV tag representation."
					options={[
						{ value: 'id3', label: 'ID3' },
						{ value: 'riff_info', label: 'RIFF INFO' },
						{ value: 'preserve_existing', label: 'Preserve existing format' }
					]}
					bind:value={formats.wav_tag_policy}
				/>
				<ToggleRow
					label="Remove stray ID3 from FLAC"
					description="Explicit compatibility cleanup."
					bind:checked={formats.remove_id3_from_flac}
				/>
				<div class="sm:col-span-2">
					<Field label="Always preserve fields (one per line)">
						{#snippet children(control)}
							<textarea
								{...control}
								class="textarea min-h-20 font-mono text-xs"
								value={metadata.preserve_fields.join('\n')}
								oninput={(event) => (metadata.preserve_fields = lines(event.currentTarget.value))}
							></textarea>
						{/snippet}
					</Field>
				</div>
			</div>
		</LibraryManagementEditorAdvanced>
		<ToggleRow
			label="Preserve permissions"
			description="Copy source file mode to the published file."
			bind:checked={fileBehavior.preserve_permissions}
		/>
		<ToggleRow
			label="Strict format capability gate"
			description="Block rather than accept documented representation loss."
			bind:checked={fileBehavior.strict_capability_gate}
		/>
		<ToggleRow
			label="Reject symlinks"
			description="Never follow linked audio or sidecar paths."
			bind:checked={fileBehavior.reject_symlinks}
		/>
		<ToggleRow
			label="Validate technical audio"
			description="Verify codec and stream properties after staging."
			bind:checked={fileBehavior.validate_technical_audio}
		/>
		<ToggleRow
			label="Automatic exact-edition acceptance"
			description="During identity checks, accept the best proven MusicBrainz edition automatically (score 0.95+, clear winner). Every acceptance is audited and can be undone."
			bind:checked={identity.automatic_edition_acceptance_enabled}
		/>
		<LibraryManagementEditorAdvanced
			class="sm:col-span-2"
			title="Post-write notifications"
			subtitle="Catalog and external media server refresh"
		>
			<div class="mt-3 grid gap-3 sm:grid-cols-2">
				<div class="management-master-toggle">
					<Check class="h-4 w-4 shrink-0 text-success" />
					<span
						><strong>DroppedNeedle catalog updates immediately</strong><small
							>Committed tags and paths are updated as part of every successful operation.</small
						></span
					>
				</div>
				<ToggleRow
					label="Refresh media servers"
					description="Notify enabled servers after commit."
					bind:checked={notification.refresh_external_servers}
				/>
			</div>
		</LibraryManagementEditorAdvanced>
	</div>
</LibraryManagementEditorSection>

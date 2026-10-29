<!--
	Preservation: scrub, preserved fields, edition acceptance and post-write
	notifications.
-->
<script lang="ts">
	import '$lib/components/library/library-management.css';
	import { Check, ShieldCheck } from 'lucide-svelte';

	import LibraryManagementEditorAdvanced from './LibraryManagementEditorAdvanced.svelte';
	import LibraryManagementEditorSection from './LibraryManagementEditorSection.svelte';
	import { lines } from './managementProfileLists';
	import type {
		LibraryManagementProfile,
		MetadataManagementSettings
	} from '$lib/queries/library-management/types';
	import TextAreaField from '$lib/ui/TextAreaField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	interface Props {
		metadata: MetadataManagementSettings;
		identity: LibraryManagementProfile['identity'];
		notification: LibraryManagementProfile['notification'];
	}

	let {
		metadata = $bindable(),
		identity = $bindable(),
		notification = $bindable()
	}: Props = $props();
</script>

<LibraryManagementEditorSection
	icon={ShieldCheck}
	title="Preservation"
	subtitle="Scrub, preserved fields, and notifications"
	active={true}
	state="Protected"
	chips={[
		...(metadata.scrub_unmanaged_tags ? [{ label: 'Scrub on', active: true }] : []),
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
		<LibraryManagementEditorAdvanced
			class="sm:col-span-2"
			title="Preserved fields"
			subtitle="Tags a write never changes"
		>
			<div class="mt-3">
				<TextAreaField
					label="Always preserve fields (one per line)"
					mono
					rows={3}
					value={metadata.preserve_fields.join('\n')}
					oninput={(text) => (metadata.preserve_fields = lines(text))}
				/>
			</div>
		</LibraryManagementEditorAdvanced>
		<div class="sm:col-span-2">
			<ToggleRow
				label="Automatic exact-edition acceptance"
				description="During identity checks, accept the best proven MusicBrainz edition automatically (score 0.95+, clear winner). Every acceptance is audited and can be undone."
				bind:checked={identity.automatic_edition_acceptance_enabled}
			/>
		</div>
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

<!-- Tag transformations: the ordered tagging scripts this profile runs. -->
<script lang="ts">
	import { Braces } from 'lucide-svelte';

	import LibraryManagementEditorSection from './LibraryManagementEditorSection.svelte';
	import LibraryManagementScriptEditor from './LibraryManagementScriptEditor.svelte';
	import type { ManagementScriptSettings } from '$lib/queries/library-management/types';

	interface Props {
		/** Every tagging script, shared by all profiles. */
		scripts: ManagementScriptSettings[];
		/** The scripts this profile runs, in order. */
		selectedIds: string[];
	}

	let { scripts = $bindable(), selectedIds = $bindable() }: Props = $props();

	const count = $derived(selectedIds.length);
</script>

<LibraryManagementEditorSection
	icon={Braces}
	title="Tag transformations"
	subtitle="Ordered metadata scripts, separate from file naming"
	active={count > 0}
	chips={[
		{
			label:
				count > 0
					? `${count} attached ${count === 1 ? 'script' : 'scripts'}`
					: 'No scripts attached'
		}
	]}
>
	<div class="mt-4">
		<LibraryManagementScriptEditor
			kind="tagging"
			{scripts}
			{selectedIds}
			onchange={(value, ids) => {
				scripts = value;
				selectedIds = ids;
			}}
		/>
	</div>
</LibraryManagementEditorSection>

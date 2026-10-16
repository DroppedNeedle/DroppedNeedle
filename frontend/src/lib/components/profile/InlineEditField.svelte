<!--
	The editor for one profile value: an input with Save and Cancel. Enter
	saves, Escape cancels. It starts from `value` each time it is shown, keeps
	itself open with the error when saving fails, and calls `ondone` once the
	save went through or the user cancelled.
-->
<script lang="ts">
	import { untrack } from 'svelte';
	import type { FullAutoFill } from 'svelte/elements';
	import { Check, X } from 'lucide-svelte';
	import { ApiError } from '$lib/api/client';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';

	interface Props {
		/** The input's accessible name, also used in "Save <label>". */
		label: string;
		value: string;
		type?: 'text' | 'email';
		placeholder?: string;
		autocomplete?: FullAutoFill;
		pending?: boolean;
		onsave: (value: string) => Promise<unknown>;
		ondone: () => void;
		class?: string;
	}

	let {
		label,
		value,
		type = 'text',
		placeholder,
		autocomplete = 'off',
		pending = false,
		onsave,
		ondone,
		class: className = ''
	}: Props = $props();

	let draft = $state(untrack(() => value));
	let error = $state<string | null>(null);

	async function save() {
		error = null;
		try {
			await onsave(draft);
			ondone();
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'Could not reach the server';
		}
	}

	function handleKeydown(event: KeyboardEvent) {
		if (event.key === 'Escape') ondone();
	}
</script>

<div class={className}>
	<form
		class="flex items-center gap-2"
		onsubmit={(event) => {
			event.preventDefault();
			void save();
		}}
	>
		<Input
			{label}
			{type}
			{placeholder}
			{autocomplete}
			size="sm"
			class="w-full max-w-xs"
			bind:value={draft}
			onkeydown={handleKeydown}
		/>
		<IconButton
			type="submit"
			label="Save {label.toLowerCase()}"
			icon={Check}
			size="sm"
			disabled={pending}
		/>
		<IconButton label="Cancel" icon={X} size="sm" onclick={ondone} />
	</form>
	{#if error}
		<p class="mt-1 text-xs text-error">{error}</p>
	{/if}
</div>

<script lang="ts">
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Input from '$lib/ui/Input.svelte';
	import Field from '$lib/ui/Field.svelte';
	import { handoff } from '../state/handoff.svelte';
</script>

<div class="space-y-4">
	<ToggleRow
		label="Enable player handoff"
		description="Move a local library queue between active players on your account."
		bind:checked={() => handoff.enabled, handoff.setEnabled}
	/>
	{#if handoff.enabled}
		<Field label="This player's name">
			{#snippet children(control)}
				<Input
					{...control}
					label="This player's name"
					value={handoff.name}
					maxlength={60}
					onchange={(event) => handoff.setName(event.currentTarget.value)}
				/>
			{/snippet}
		</Field>
		{#each handoff.players as peer (peer.id)}
			<div class="flex flex-wrap items-center gap-2">
				<span class="me-auto font-medium">{peer.name}</span>
				<Button
					size="sm"
					disabled={handoff.pending || !handoff.available}
					aria-label={'Move queue to ' + peer.name}
					onclick={() => handoff.move(peer.id, 'send')}>Move there</Button
				>
				<Button
					size="sm"
					disabled={handoff.pending || !peer.can_send}
					aria-label={'Continue queue from ' + peer.name + ' here'}
					onclick={() => handoff.move(peer.id, 'receive')}>Continue here</Button
				>
			</div>
		{:else}
			<p class="text-sm text-muted">
				Enable handoff in another player on this account to see it here.
			</p>
		{/each}
		{#if handoff.players.length && !handoff.available}
			<p class="text-sm text-muted">To send from here, load a queue of up to 2,000 local tracks.</p>
		{/if}
		<p role="status" class="text-sm">{handoff.notice}</p>
	{/if}
</div>

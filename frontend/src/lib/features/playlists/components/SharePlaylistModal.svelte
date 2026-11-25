<!--
	Who a playlist is for: only its owner, people chosen from the server's
	users, or everyone. The chosen people are listed, each with whether they
	may edit its songs, and more are found by name, so it reads the same
	with three users or three hundred. Saving sets the visibility and the
	people together. Choosing Everyone leaves the chosen people as they were.
-->
<script lang="ts">
	import { Plus, Search, Users, X } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import RadioCards from '$lib/ui/RadioCards.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import {
		getPlaylistMembersV3Query,
		getUserDirectoryV3Query
	} from '$lib/features/playlists/queries/PlaylistV3Queries.svelte';
	import {
		createSetPlaylistMembersV3Mutation,
		createSetPlaylistVisibilityV3Mutation
	} from '$lib/features/playlists/queries/PlaylistV3Mutations.svelte';

	interface Props {
		playlistId: string;
		isPublic: boolean;
		onsaved: (isPublic: boolean) => void;
	}

	let { playlistId, isPublic, onsaved }: Props = $props();

	type Audience = 'private' | 'people' | 'everyone';
	type Person = { id: string; display_name: string };
	type Member = Person & { can_edit: boolean };
	const audiences: { value: Audience; label: string; description: string }[] = [
		{
			value: 'private',
			label: 'Only me',
			description: 'Others see it locked, with its track count and any display name you set.'
		},
		{ value: 'people', label: 'Specific people', description: 'Only the people you add.' },
		{ value: 'everyone', label: 'Everyone', description: 'Every user of this server.' }
	];
	/** Matches shown at once; more letters narrow the rest. */
	const MATCHES = 6;

	let open = $state(false);
	let audience = $state<Audience>('private');
	let chosen = $state<Member[]>([]);
	let query = $state('');
	let seeded = $state(false);

	const members = getPlaylistMembersV3Query(
		() => playlistId,
		() => open
	);
	const people = getUserDirectoryV3Query(() => open);
	const setMembers = createSetPlaylistMembersV3Mutation();
	const setVisibility = createSetPlaylistVisibilityV3Mutation();

	let saving = $derived(setMembers.isPending || setVisibility.isPending);

	export function showModal() {
		seeded = false;
		query = '';
		open = true;
	}

	// Seeds once per opening, from the people it is shared with now.
	$effect(() => {
		const current = members.data;
		if (!open || seeded || !current) return;
		seeded = true;
		chosen = current.map((member) => ({
			id: member.user_id,
			display_name: member.display_name,
			can_edit: member.can_edit
		}));
		audience = isPublic ? 'everyone' : current.length > 0 ? 'people' : 'private';
	});

	function matches(users: Person[]): Person[] {
		const wanted = query.trim().toLowerCase();
		if (!wanted) return [];
		return users
			.filter(
				(user) =>
					user.id !== authStore.user?.id &&
					!chosen.some((person) => person.id === user.id) &&
					user.display_name.toLowerCase().includes(wanted)
			)
			.slice(0, MATCHES);
	}

	function add(user: Person) {
		chosen = [...chosen, { ...user, can_edit: false }];
		query = '';
	}

	async function save() {
		const nowPublic = audience === 'everyone';
		try {
			if (nowPublic !== isPublic) {
				await setVisibility.mutateAsync({ id: playlistId, isPublic: nowPublic });
			}
			if (!nowPublic) {
				const sharedWith = audience === 'people' ? chosen : [];
				await setMembers.mutateAsync({
					id: playlistId,
					userIds: sharedWith.map((person) => person.id),
					editorIds: sharedWith.filter((person) => person.can_edit).map((person) => person.id)
				});
			}
			onsaved(nowPublic);
			open = false;
			toastStore.show({ message: 'Sharing updated', type: 'success' });
		} catch {
			toastStore.show({ message: "Couldn't update sharing", type: 'error' });
		}
	}
</script>

<Modal bind:open title="Share playlist" icon={Users} size="md" closeButton>
	<div class="flex flex-col gap-4">
		<RadioCards label="Who can see this playlist" options={audiences} bind:value={audience} />
		{#if audience === 'people'}
			<fieldset class="flex flex-col gap-3">
				<legend class="mb-3 font-mono text-xs font-semibold tracking-[0.16em] text-muted uppercase">
					Shared with
				</legend>
				{#each chosen as person (person.id)}
					<div class="flex items-center gap-3 text-sm font-medium">
						<span class="min-w-0 flex-1 truncate">{person.display_name}</span>
						<label class="flex shrink-0 items-center gap-2 text-xs font-normal text-muted">
							Can edit
							<ToggleRow
								bare
								label="{person.display_name} can edit"
								bind:checked={person.can_edit}
							/>
						</label>
						<IconButton
							size="xs"
							icon={X}
							label="Remove {person.display_name}"
							onclick={() => (chosen = chosen.filter((other) => other.id !== person.id))}
						/>
					</div>
				{:else}
					<p class="text-sm text-muted">No one yet. Add people by name below.</p>
				{/each}
			</fieldset>
			<QueryBoundary query={people} what="this server's users">
				{#snippet children(users)}
					<div class="flex flex-col gap-2">
						<Input
							label="Add people"
							placeholder="Add people by name"
							icon={Search}
							class="w-full"
							bind:value={query}
						/>
						{#each matches(users) as user (user.id)}
							<div class="flex items-center justify-between gap-3 px-1 text-sm">
								<span class="truncate">{user.display_name}</span>
								<IconButton
									size="xs"
									icon={Plus}
									label="Add {user.display_name}"
									onclick={() => add(user)}
								/>
							</div>
						{:else}
							{#if query.trim()}
								<p class="px-1 text-sm text-muted">
									No one else by that name. People show up here once they save a display name under
									Profile.
								</p>
							{/if}
						{/each}
					</div>
				{/snippet}
			</QueryBoundary>
		{/if}
	</div>
	{#snippet actions()}
		<Button size="sm" onclick={() => (open = false)} disabled={saving}>Cancel</Button>
		<Button
			variant="primary"
			size="sm"
			loading={saving}
			disabled={!seeded || (audience === 'people' && chosen.length === 0)}
			onclick={() => void save()}
		>
			Save
		</Button>
	{/snippet}
</Modal>

-- bedwars_core.luau: demo Bedwars mode for pocketcore.
--
-- Shows: team spawns, iron/gold generator timers, block placement protection,
-- bed rules, friendly-fire protection and a /bw command.
--
-- Conventions: a handler returns `true` to CANCEL the event. Player handles
-- are userdata; compare players by player:get_eid(), never by handle.
-- Edit and save this file while the server runs: it hot-reloads (state resets).

local IRON, GOLD = 265, 266
local BED = 26

local IRON_EVERY = 30      -- ticks (20 ticks = 1 second)
local GOLD_EVERY = 160
local MIN_PLAYERS = 2
local BUILD_Y_MAX = 40

local LOBBY = { x = 0.5, y = 4, z = 0.5 }

-- Coordinates assume the default flat world (floor top at y = 3).
local TEAMS = {
	{ name = "Red",  tag = "§c", wool = 14,
	  spawn = { x = -28.5, y = 4, z = 0.5 }, gen = { x = -30.5, y = 4, z = 0.5 }, bed = { x = -26, y = 4, z = 0 } },
	{ name = "Blue", tag = "§9", wool = 11,
	  spawn = { x = 29.5, y = 4, z = 0.5 },  gen = { x = 31.5, y = 4, z = 0.5 },  bed = { x = 27, y = 4, z = 0 } },
}

local phase = "lobby"      -- "lobby" | "running"
local roster = {}          -- eid -> { player, team, alive }
local bed_alive = {}       -- team index -> bool
local placed = {}          -- "x:y:z" -> true for blocks players placed

local function key(x, y, z)
	return x .. ":" .. y .. ":" .. z
end

local function say(msg)
	server:broadcast(msg)
end

local function within(x, z, p, r)
	return math.abs(x - p.x) <= r and math.abs(z - p.z) <= r
end

local function count(t)
	local n = 0
	for _ in pairs(t) do
		n += 1
	end
	return n
end

local function pick_team()
	local counts = {}
	for i = 1, #TEAMS do
		counts[i] = 0
	end
	for _, e in pairs(roster) do
		counts[e.team] += 1
	end
	local best = 1
	for i = 2, #TEAMS do
		if counts[i] < counts[best] then
			best = i
		end
	end
	return best
end

local function find_team(name)
	name = name:lower()
	for i, t in ipairs(TEAMS) do
		if t.name:lower() == name then
			return i
		end
	end
	return nil
end

local function equip(e)
	local p, t = e.player, TEAMS[e.team]
	p:clear_inventory()
	p:give_item(268, 1)                 -- wooden sword
	p:give_item(35, 16, t.wool)         -- team wool
	p:set_armor(298, 299, 300, 301)     -- leather set
	p:set_gamemode(0)
	p:set_health(20)
end

local function spawn_at(e)
	local s = TEAMS[e.team].spawn
	e.player:set_spawn(s.x, s.y, s.z)
	e.player:teleport(s.x, s.y, s.z)
end

local function to_lobby(player)
	player:set_spawn(LOBBY.x, LOBBY.y, LOBBY.z)
	player:teleport(LOBBY.x, LOBBY.y, LOBBY.z)
	player:set_gamemode(2)
	player:clear_inventory()
	player:set_armor(0, 0, 0, 0)
end

local function stop_game(reason)
	if phase ~= "running" then
		return
	end
	phase = "lobby"
	if reason then
		say("§6[Bedwars] §e" .. reason)
	end
	for _, e in pairs(roster) do
		if e.player:is_online() then
			to_lobby(e.player)
		end
	end
	roster = {}
	placed = {}
	world:reset_arena() -- restores every block and clears dropped items
end

local function check_winner()
	local alive_teams, last = 0, nil
	for i = 1, #TEAMS do
		for _, e in pairs(roster) do
			if e.team == i and e.alive then
				alive_teams += 1
				last = i
				break
			end
		end
	end
	if alive_teams <= 1 then
		if last then
			stop_game(TEAMS[last].tag .. TEAMS[last].name .. " team wins!")
		else
			stop_game("Game over.")
		end
	end
end

local function start_game()
	phase = "running"
	placed = {}
	world:reset_arena()
	for i, t in ipairs(TEAMS) do
		bed_alive[i] = true
		world:set_block(t.bed.x, t.bed.y, t.bed.z, BED, 0)
	end
	for _, e in pairs(roster) do
		e.alive = true
		equip(e)
		spawn_at(e)
	end
	say("§6[Bedwars] §eThe game has started! Protect your bed, destroy theirs!")
end

------------------------------------------------------------------ events

function on_server_start()
	log("bedwars_core loaded (" .. #TEAMS .. " teams)")
end

function on_tick(tick)
	if phase ~= "running" then
		return
	end
	if tick % IRON_EVERY == 0 then
		for _, t in ipairs(TEAMS) do
			world:spawn_item(t.gen.x, t.gen.y + 0.3, t.gen.z, IRON, 1)
		end
	end
	if tick % GOLD_EVERY == 0 then
		for _, t in ipairs(TEAMS) do
			world:spawn_item(t.gen.x, t.gen.y + 0.3, t.gen.z, GOLD, 1)
		end
	end
end

function on_player_join(player)
	to_lobby(player)
	player:send_message("§aWelcome to Bedwars! §7Type /bw join, then /bw start.")
end

function on_player_quit(player)
	local eid = player:get_eid()
	if roster[eid] then
		roster[eid] = nil
		if phase == "running" then
			check_winner()
		end
	end
end

function on_block_place(player, pos, id, meta)
	local e = roster[player:get_eid()]
	if phase ~= "running" or not e or not e.alive then
		return true
	end
	local x, y, z = math.floor(pos.x), math.floor(pos.y), math.floor(pos.z)
	if y > BUILD_Y_MAX then
		player:send_popup("§cBuild height limit reached")
		return true
	end
	for _, t in ipairs(TEAMS) do
		if within(x, z, t.spawn, 3) or within(x, z, t.gen, 2) then
			player:send_popup("§cYou cannot build here")
			return true
		end
	end
	placed[key(x, y, z)] = true
	return false
end

function on_block_break(player, pos)
	local e = roster[player:get_eid()]
	if phase ~= "running" or not e or not e.alive then
		return true
	end
	local x, y, z = math.floor(pos.x), math.floor(pos.y), math.floor(pos.z)
	for i, t in ipairs(TEAMS) do
		if t.bed.x == x and t.bed.y == y and t.bed.z == z then
			if i == e.team then
				player:send_popup("§cYou cannot break your own bed!")
				return true
			end
			bed_alive[i] = false
			say(string.format("§6[Bedwars] %s%s§e's bed was destroyed by %s!", t.tag, t.name, player:get_name() or "?"))
			return false
		end
	end
	if placed[key(x, y, z)] then
		placed[key(x, y, z)] = nil
		return false
	end
	player:send_popup("§cYou can only break blocks placed by players")
	return true
end

function on_player_damage(victim, attacker, damage)
	local v = roster[victim:get_eid()]
	if phase ~= "running" or not v or not v.alive then
		return true
	end
	if attacker then
		local a = roster[attacker:get_eid()]
		if not a or not a.alive or a.team == v.team then
			return true -- no lobby fights, no friendly fire
		end
	end
	return nil
end

function on_player_death(victim, killer)
	local e = roster[victim:get_eid()]
	if not e then
		return
	end
	local who = victim:get_name() or "?"
	if killer then
		say(string.format("§6[Bedwars] §e%s was slain by %s", who, killer:get_name() or "?"))
	else
		say("§6[Bedwars] §e" .. who .. " died")
	end
	if not bed_alive[e.team] then
		e.alive = false
		victim:send_message("§cYour bed is gone: you are eliminated.")
		check_winner()
	end
end

function on_player_respawn(player)
	local e = roster[player:get_eid()]
	if not e then
		return
	end
	if e.alive then
		equip(e)
	else
		to_lobby(player)
	end
end

------------------------------------------------------------------ /bw

local function help(p)
	p:send_message("§e/bw join [red|blue] §7- join the next game")
	p:send_message("§e/bw leave §7- leave the game")
	p:send_message("§e/bw start §7- start (needs " .. MIN_PLAYERS .. " players)")
	p:send_message("§e/bw stop §7- end the game")
	p:send_message("§e/bw status")
end

function on_player_chat(player, msg)
	local cmd, arg = msg:match("^/bw%s*(%S*)%s*(%S*)")
	if not cmd then
		return false -- normal chat
	end
	local eid = player:get_eid()

	if cmd == "join" then
		if phase ~= "lobby" then
			player:send_message("§cA game is already running.")
		elseif roster[eid] then
			player:send_message("§cYou already joined.")
		else
			local team = find_team(arg) or pick_team()
			roster[eid] = { player = player, team = team, alive = true }
			say(string.format("§6[Bedwars] §e%s joined %s%s", player:get_name() or "?", TEAMS[team].tag, TEAMS[team].name))
		end
	elseif cmd == "leave" then
		if roster[eid] then
			roster[eid] = nil
			to_lobby(player)
			if phase == "running" then
				check_winner()
			end
		end
	elseif cmd == "start" then
		if phase ~= "lobby" then
			player:send_message("§cA game is already running.")
		elseif count(roster) < MIN_PLAYERS then
			player:send_message("§cNeed at least " .. MIN_PLAYERS .. " players (/bw join).")
		else
			start_game()
		end
	elseif cmd == "stop" then
		stop_game("The game was stopped.")
	elseif cmd == "status" then
		player:send_message(string.format("§ePhase: %s, players: %d", phase, count(roster)))
		for i, t in ipairs(TEAMS) do
			local n = 0
			for _, e in pairs(roster) do
				if e.team == i and e.alive then
					n += 1
				end
			end
			player:send_message(string.format("%s%s§r: %d alive, bed %s", t.tag, t.name, n, bed_alive[i] == false and "gone" or "intact"))
		end
	else
		help(player)
	end
	return true -- command handled; do not broadcast as chat
end

-- The options dialog (Options in the main menu), as the engine's GameUI makes it: a frame of
-- the Source scheme (resource/sourcescheme.res), its pages laid out by the game's own
-- resource/optionssub*.res and their controls joined to the console variables GameUI joins
-- them to. What is changed waits: OK and Apply keep it (OpenGMOD.SetSetting,
-- OpenGMOD.BindKey), Cancel forgets it.

local Options = {}
OpenGMODOptions = Options

-- the panels are registered when the dialog first opens: the menu's Lua (vgui.Register) is
-- not there yet when this runs
local registered, toRegister = false, {}
local function Register( name, panel, base ) toRegister[ #toRegister + 1 ] = { name, panel, base } end
local function RegisterAll()
	if ( registered ) then return end
	registered = true
	Options.MakeScheme()
	for _, r in ipairs( toRegister ) do vgui.Register( r[ 1 ], r[ 2 ], r[ 3 ] ) end
end

-- the colours of SourceScheme's BaseSettings
local S
function Options.MakeScheme()
	S = {
	frame = Color( 108, 111, 114, 250 ),
	title = Color( 255, 255, 255, 204 ),
	label = Color( 221, 221, 221 ),
	disabled = Color( 110, 110, 110 ),
	disabled2 = Color( 50, 50, 50 ),
	bright = Color( 200, 200, 200 ),
	dark = Color( 40, 40, 40, 198 ),
	subtle = Color( 80, 80, 80 ),
	button = Color( 227, 227, 227 ),
	button_text = Color( 82, 82, 82 ),
	button_armed = Color( 240, 240, 240 ),
	button_armed_text = Color( 46, 114, 178 ),
	button_down = Color( 84, 178, 245 ),
	white = Color( 255, 255, 255 ),
	black = Color( 0, 0, 0 ),
	check_disabled = Color( 180, 180, 180 ),
	entry_text = Color( 10, 10, 10 ),
	entry_disabled = Color( 192, 192, 192 ),
	entry_disabled_text = Color( 128, 128, 128 ),
	selected = Color( 0, 168, 255, 204 ),
	track = Color( 184, 184, 184, 229 ),
	nob = Color( 220, 220, 220 ),
	arrow = Color( 81, 81, 81 ),
	menu = Color( 233, 233, 233 ),
	menu_text = Color( 80, 80, 80 ),
	menu_armed = Color( 132, 183, 241 ),
	list_header = Color( 50, 50, 50 ),
	list_divider = Color( 0, 0, 0, 150 ),
	scroll_button = Color( 207, 207, 207 ),
	scroll_arrow = Color( 60, 60, 60 ),
	tab_text = Color( 10, 10, 10, 200 ),
}
end

local function Fonts()
	if ( Options.Fonts ) then return end
	Options.Fonts = true
	surface.CreateFont( "OpenGMODSource", { font = "Tahoma", size = 13, weight = 500 } )
	surface.CreateFont( "OpenGMODSourceSmall", { font = "Tahoma", size = 11, weight = 0 } )
	surface.CreateFont( "OpenGMODSourceBold", { font = "Tahoma", size = 12, weight = 1000 } )
end

-- what the engine's own phrases say; some are GMod's alone, or not in every language
local FALLBACK = {
	GameUI_WindowedNoBorder = "Windowed (no border)",
	GameUI_AspectAny = "Any/All",
	GAMEUI_RequiresRestart = "(Requires restart)",
}
local function Phrase( text )
	if ( !text or text == "" ) then return "" end
	if ( text[ 1 ] != "#" ) then return text end
	local phrase = language.GetPhrase( text )
	if ( phrase == text or phrase == text:sub( 2 ) ) then return FALLBACK[ text:sub( 2 ) ] or phrase end
	return phrase
end

-- what was changed and not kept yet
local Pending
local function Get( name, default )
	local value = Pending.values[ name ]
	if ( value == nil ) then value = OpenGMOD.GetSetting( name ) end
	if ( value == nil ) then return default end
	return value
end
local function GetNumber( name, default ) return tonumber( Get( name, default ) ) or default end
local function Set( name, value )
	Pending.values[ name ] = tostring( value )
	if ( Options.OnChanged ) then Options.OnChanged() end
end

local function Number( text, default )
	if ( !text or text == "" ) then return default end
	local sign, rest = text:match( "^([rc]?)(-?[%d%.]+)$" )
	return tonumber( rest ) or default, sign
end

------------------------------------------------------------------------------------------------
-- the controls of vgui_controls, drawn as the Source scheme draws them

local function Raised( w, h, fill )
	surface.SetDrawColor( fill )
	surface.DrawRect( 0, 0, w, h )
	surface.SetDrawColor( S.bright.r, S.bright.g, S.bright.b, 120 )
	surface.DrawRect( 0, 0, w, 1 )
	surface.DrawRect( 0, 0, 1, h )
	surface.SetDrawColor( S.dark )
	surface.DrawRect( 0, h - 1, w, 1 )
	surface.DrawRect( w - 1, 0, 1, h )
end

local function Sunken( w, h, fill )
	surface.SetDrawColor( fill )
	surface.DrawRect( 0, 0, w, h )
	surface.SetDrawColor( S.dark )
	surface.DrawRect( 0, 0, w, 1 )
	surface.DrawRect( 0, 0, 1, h )
	surface.SetDrawColor( S.bright.r, S.bright.g, S.bright.b, 90 )
	surface.DrawRect( 0, h - 1, w, 1 )
	surface.DrawRect( w - 1, 0, 1, h )
end

local function Text( text, font, color, x, y, align_x, align_y )
	draw.SimpleText( text, font, x, y, color, align_x or TEXT_ALIGN_LEFT, align_y or TEXT_ALIGN_CENTER )
end

-- a label, embossed when switched off
local LABEL = {}
function LABEL:Init()
	self.Text = ""
	self.AlignX, self.AlignY = TEXT_ALIGN_LEFT, TEXT_ALIGN_CENTER
	self:SetMouseInputEnabled( false )
end
function LABEL:SetLabel( text, wrap ) self.Text, self.Wrap = text, wrap end
function LABEL:Paint( w, h )
	local enabled = self:IsEnabled()
	local color = self.Color or ( enabled and S.label or S.disabled )
	if ( self.Wrap ) then
		surface.SetFont( "OpenGMODSource" )
		local lines, line = {}, ""
		for word in self.Text:gmatch( "%S+" ) do
			local try = line == "" and word or ( line .. " " .. word )
			if ( surface.GetTextSize( try ) > w and line != "" ) then
				lines[ #lines + 1 ] = line
				line = word
			else
				line = try
			end
		end
		lines[ #lines + 1 ] = line
		for i, text in ipairs( lines ) do Text( text, "OpenGMODSource", color, 0, ( i - 1 ) * 14, TEXT_ALIGN_LEFT, TEXT_ALIGN_TOP ) end
		return
	end
	local x = self.AlignX == TEXT_ALIGN_CENTER and w / 2 or self.AlignX == TEXT_ALIGN_RIGHT and w or 0
	local y = self.AlignY == TEXT_ALIGN_TOP and 0 or h / 2
	if ( !enabled and !self.Color ) then Text( self.Text, "OpenGMODSource", S.disabled2, x + 1, y + 1, self.AlignX, self.AlignY ) end
	Text( self.Text, "OpenGMODSource", color, x, y, self.AlignX, self.AlignY )
end
Register( "OpenGMODSourceLabel", LABEL, "Panel" )

-- a button
local BUTTON = {}
function BUTTON:Init()
	self.Text = ""
	self:SetMouseInputEnabled( true )
	self:SetCursor( "hand" )
end
function BUTTON:SetLabel( text ) self.Text = text end
function BUTTON:OnMousePressed( code )
	if ( code != MOUSE_LEFT or !self:IsEnabled() ) then return end
	self.Down = true
	self:MouseCapture( true )
end
function BUTTON:OnMouseReleased( code )
	if ( code != MOUSE_LEFT or !self.Down ) then return end
	self.Down = false
	self:MouseCapture( false )
	if ( self:IsHovered() and self.DoClick ) then self:DoClick() end
end
function BUTTON:Paint( w, h )
	local enabled = self:IsEnabled()
	local down = self.Down and self:IsHovered()
	local armed = enabled and self:IsHovered()
	if ( self.Link ) then
		Raised( w, h, armed and S.button_armed or S.button )
	elseif ( down ) then
		Sunken( w, h, S.button_down )
	else
		Raised( w, h, armed and S.button_armed or S.button )
	end
	if ( self.Default ) then
		surface.SetDrawColor( S.button_text )
		surface.DrawOutlinedRect( 0, 0, w, h )
	end
	local color = !enabled and S.entry_disabled_text or down and S.white or armed and S.button_armed_text or S.button_text
	local x = self.Link and 6 or w / 2
	Text( self.Text, "OpenGMODSource", color, x, h / 2, self.Link and TEXT_ALIGN_LEFT or TEXT_ALIGN_CENTER )
	if ( self.Link ) then
		surface.SetFont( "OpenGMODSource" )
		local tw, th = surface.GetTextSize( self.Text )
		surface.SetDrawColor( color )
		surface.DrawRect( x, h / 2 + th / 2 - 1, tw, 1 )
	end
end
Register( "OpenGMODSourceButton", BUTTON, "Panel" )

-- a check button: the box, and the text after it
local CHECK = {}
function CHECK:Init()
	self.Text = ""
	self.Checked = false
	self:SetMouseInputEnabled( true )
end
function CHECK:SetLabel( text ) self.Text = text end
function CHECK:SetChecked( on ) self.Checked = on and true or false end
function CHECK:OnMousePressed( code )
	if ( code != MOUSE_LEFT or !self:IsEnabled() ) then return end
	self.Checked = !self.Checked
	if ( self.OnChange ) then self:OnChange( self.Checked ) end
end
function CHECK:Paint( w, h )
	local enabled = self:IsEnabled()
	local size = 13
	local x, y = 3, math.floor( ( h - size ) / 2 )
	surface.SetDrawColor( enabled and S.white or S.check_disabled )
	surface.DrawRect( x, y, size, size )
	surface.SetDrawColor( S.subtle )
	surface.DrawOutlinedRect( x, y, size, size )
	if ( self.Checked ) then
		surface.SetDrawColor( enabled and S.black or S.disabled2 )
		for i = 0, 1 do
			surface.DrawLine( x + 3, y + 6 + i, x + 5, y + 8 + i )
			surface.DrawLine( x + 5, y + 8 + i, x + 10, y + 3 + i )
		end
	end
	if ( !enabled ) then Text( self.Text, "OpenGMODSource", S.disabled2, x + size + 7, h / 2 + 1 ) end
	Text( self.Text, "OpenGMODSource", enabled and S.label or S.disabled, x + size + 6, h / 2 )
end
Register( "OpenGMODSourceCheck", CHECK, "Panel" )

-- a divider: a line cut in
local DIVIDER = {}
function DIVIDER:Paint( w, h )
	surface.SetDrawColor( S.dark )
	surface.DrawRect( 0, 0, w, 1 )
	surface.SetDrawColor( S.bright.r, S.bright.g, S.bright.b, 70 )
	surface.DrawRect( 0, 1, w, 1 )
end
Register( "OpenGMODSourceDivider", DIVIDER, "Panel" )

-- a combo box: the choice, and the list of them under it
local COMBO = {}
function COMBO:Init()
	self.Items = {}
	self.Selected = nil
	self:SetMouseInputEnabled( true )
end
function COMBO:AddChoice( text, data ) self.Items[ #self.Items + 1 ] = { text = text, data = data } end
function COMBO:Clear() self.Items, self.Selected = {}, nil end
function COMBO:ChooseData( data )
	for i, item in ipairs( self.Items ) do
		if ( tostring( item.data ) == tostring( data ) ) then self.Selected = i return true end
	end
	return false
end
function COMBO:GetSelected() local item = self.Items[ self.Selected or 0 ] return item and item.data, item and item.text end
function COMBO:OnMousePressed( code )
	if ( code != MOUSE_LEFT or !self:IsEnabled() or #self.Items == 0 ) then return end
	if ( IsValid( self.List ) ) then self.List:Remove() return end
	local x, y = self:LocalToScreen( 0, self:GetTall() )
	local list = vgui.Create( "Panel" )
	list:SetPos( x, y )
	list:SetSize( self:GetWide(), #self.Items * 18 + 2 )
	list:MakePopup()
	list:SetKeyboardInputEnabled( false )
	list.Paint = function( p, w, h )
		surface.SetDrawColor( S.menu )
		surface.DrawRect( 0, 0, w, h )
		surface.SetDrawColor( S.subtle )
		surface.DrawOutlinedRect( 0, 0, w, h )
	end
	list.Think = function( p )
		-- a click anywhere else closes it
		if ( input.IsMouseDown( MOUSE_LEFT ) and !p:IsHovered() and !p:IsChildHovered() and !self:IsHovered() ) then p:Remove() end
	end
	for i, item in ipairs( self.Items ) do
		local row = list:Add( "Panel" )
		row:SetPos( 1, 1 + ( i - 1 ) * 18 )
		row:SetSize( self:GetWide() - 2, 18 )
		row:SetCursor( "hand" )
		row.Paint = function( r, w, h )
			local armed = r:IsHovered()
			if ( armed ) then
				surface.SetDrawColor( S.menu_armed )
				surface.DrawRect( 0, 0, w, h )
			end
			Text( item.text, "OpenGMODSource", armed and S.white or S.menu_text, 6, h / 2 )
		end
		row.OnMousePressed = function()
			self.Selected = i
			list:Remove()
			if ( self.OnSelect ) then self:OnSelect( item.data, item.text ) end
		end
	end
	self.List = list
end
function COMBO:OnRemove() if ( IsValid( self.List ) ) then self.List:Remove() end end
function COMBO:Paint( w, h )
	local enabled = self:IsEnabled()
	surface.SetDrawColor( enabled and S.white or S.entry_disabled )
	surface.DrawRect( 0, 0, w, h )
	surface.SetDrawColor( S.dark )
	surface.DrawOutlinedRect( 0, 0, w, h )
	local _, text = self:GetSelected()
	Text( text or "", "OpenGMODSource", enabled and S.entry_text or S.entry_disabled_text, 4, h / 2 )
	-- the arrow, pointing down
	local ax, ay = w - 12, math.floor( h / 2 ) - 2
	surface.SetDrawColor( enabled and S.arrow or S.entry_disabled_text )
	for i = 0, 3 do surface.DrawRect( ax - 4 + i, ay + i, 9 - i * 2, 1 ) end
end
Register( "OpenGMODSourceCombo", COMBO, "Panel" )

-- a slider: the track and its ticks, the nob, and what the ends are
local SLIDER = {}
function SLIDER:Init()
	self.Min, self.Max, self.Value = 0, 1, 0
	self.Ticks = 10
	self.LeftText, self.RightText = "", ""
	self:SetMouseInputEnabled( true )
end
function SLIDER:SetRange( min, max ) self.Min, self.Max = min, max end
function SLIDER:SetValue( value ) self.Value = math.Clamp( value, math.min( self.Min, self.Max ), math.max( self.Min, self.Max ) ) end
function SLIDER:Track() return 5, self:GetWide() - 10 end
function SLIDER:ValueAt( x )
	local left, width = self:Track()
	local f = math.Clamp( ( x - left ) / width, 0, 1 )
	return self.Min + ( self.Max - self.Min ) * f
end
function SLIDER:OnMousePressed( code )
	if ( code != MOUSE_LEFT or !self:IsEnabled() ) then return end
	self.Dragging = true
	self:MouseCapture( true )
	self:OnCursorMoved( self:CursorPos() )
end
function SLIDER:OnMouseReleased()
	self.Dragging = false
	self:MouseCapture( false )
end
function SLIDER:OnCursorMoved( x )
	if ( !self.Dragging ) then return end
	self:SetValue( self:ValueAt( x ) )
	if ( self.OnValueChanged ) then self:OnValueChanged( self.Value ) end
end
function SLIDER:Paint( w, h )
	local enabled = self:IsEnabled()
	local left, width = self:Track()
	local y = 8
	-- the track, cut in
	surface.SetDrawColor( S.dark )
	surface.DrawRect( left, y, width, 1 )
	surface.SetDrawColor( S.track )
	surface.DrawRect( left, y + 1, width, 3 )
	surface.SetDrawColor( S.bright.r, S.bright.g, S.bright.b, 90 )
	surface.DrawRect( left, y + 4, width, 1 )
	-- the ticks
	surface.SetDrawColor( enabled and S.disabled2 or S.disabled )
	for i = 0, self.Ticks do
		local x = math.floor( left + width * i / self.Ticks )
		surface.DrawRect( x, y + 9, 1, 4 )
	end
	-- the ends
	local color = enabled and S.disabled2 or S.disabled
	Text( self.LeftText, "OpenGMODSourceSmall", color, left, y + 21, TEXT_ALIGN_LEFT, TEXT_ALIGN_TOP )
	Text( self.RightText, "OpenGMODSourceSmall", color, left + width, y + 21, TEXT_ALIGN_RIGHT, TEXT_ALIGN_TOP )
	-- the nob
	local f = ( self.Max != self.Min ) and ( self.Value - self.Min ) / ( self.Max - self.Min ) or 0
	local nx = math.floor( left + width * f )
	surface.SetDrawColor( enabled and S.nob or S.check_disabled )
	surface.DrawRect( nx - 3, y - 6, 7, 16 )
	surface.SetDrawColor( S.bright )
	surface.DrawRect( nx - 3, y - 6, 7, 1 )
	surface.DrawRect( nx - 3, y - 6, 1, 16 )
	surface.SetDrawColor( S.dark )
	surface.DrawRect( nx - 3, y + 9, 7, 1 )
	surface.DrawRect( nx + 3, y - 6, 1, 16 )
end
Register( "OpenGMODSourceSlider", SLIDER, "Panel" )

-- a text entry
local ENTRY = {}
function ENTRY:Init()
	self:SetFont( "OpenGMODSource" )
	self:SetPaintBackground( false )
end
function ENTRY:Paint( w, h )
	local enabled = self:IsEnabled()
	surface.SetDrawColor( enabled and S.white or S.entry_disabled )
	surface.DrawRect( 0, 0, w, h )
	surface.SetDrawColor( self:HasFocus() and S.black or S.dark )
	surface.DrawOutlinedRect( 0, 0, w, h )
	self:DrawTextEntryText( enabled and S.entry_text or S.entry_disabled_text, S.selected, S.entry_text )
end
Register( "OpenGMODSourceEntry", ENTRY, "DTextEntry" )

------------------------------------------------------------------------------------------------
-- the frame, and its sheet of pages

local FRAME = {}
function FRAME:Init()
	Fonts()
	self.Title = ""
	self:SetMouseInputEnabled( true )
	self:SetKeyboardInputEnabled( true )
	local close = self:Add( "Panel" )
	close:SetSize( 18, 18 )
	close:SetCursor( "hand" )
	close.Paint = function( p, w, h )
		local color = p:IsHovered() and S.white or S.bright
		surface.SetDrawColor( color )
		for i = 0, 1 do
			surface.DrawLine( 5 + i, 5, 13 + i, 13 )
			surface.DrawLine( 13 + i, 5, 5 + i, 13 )
		end
	end
	close.OnMousePressed = function() self:Close() end
	self.CloseButton = close
end
function FRAME:SetLabel( text ) self.Title = text end
function FRAME:PerformLayout( w, h ) self.CloseButton:SetPos( w - 26, 6 ) end
function FRAME:OnMousePressed( code )
	local x, y = self:CursorPos()
	if ( code == MOUSE_LEFT and y < 26 ) then
		self.Drag = { gui.MouseX() - self.x, gui.MouseY() - self.y }
		self:MouseCapture( true )
	end
end
function FRAME:OnMouseReleased()
	self.Drag = nil
	self:MouseCapture( false )
end
function FRAME:Think()
	if ( self.Drag ) then
		self:SetPos( math.Clamp( gui.MouseX() - self.Drag[ 1 ], 0, ScrW() - self:GetWide() ), math.Clamp( gui.MouseY() - self.Drag[ 2 ], 0, ScrH() - self:GetTall() ) )
	end
end
function FRAME:Close()
	if ( self.OnClose ) then self:OnClose() end
	self:Remove()
end
function FRAME:Paint( w, h )
	surface.SetDrawColor( S.frame )
	surface.DrawRect( 0, 0, w, h )
	surface.SetDrawColor( 0, 0, 0, 120 )
	surface.DrawOutlinedRect( 0, 0, w, h )
	Text( string.upper( self.Title ), "OpenGMODSourceBold", S.title, 16, 15 )
end
Register( "OpenGMODSourceFrame", FRAME, "EditablePanel" )

-- the tabs over the pages
local function MakeSheet( frame, x, y, w, h )
	local sheet = frame:Add( "Panel" )
	sheet:SetPos( x, y )
	sheet:SetSize( w, h )
	sheet.Tabs, sheet.Pages = {}, {}
	local TAB = 24
	sheet.Paint = function( p, pw, ph )
		-- the pages' ground, under the tabs
		surface.SetDrawColor( S.frame )
		surface.DrawRect( 0, TAB, pw, ph - TAB )
		surface.SetDrawColor( S.dark )
		surface.DrawOutlinedRect( 0, TAB - 1, pw, ph - TAB + 1 )
	end
	function sheet:AddPage( name, page )
		local tab = self:Add( "Panel" )
		surface.SetFont( "OpenGMODSource" )
		local tw = surface.GetTextSize( name )
		local x = 0
		for _, other in ipairs( self.Tabs ) do x = x + other:GetWide() + 1 end
		tab:SetPos( x, 0 )
		tab:SetSize( math.max( tw + 22, 50 ), TAB )
		tab:SetCursor( "hand" )
		tab.Paint = function( t, w, h )
			local active = self.Active == page
			surface.SetDrawColor( active and S.frame or Color( 92, 95, 98, 250 ) )
			surface.DrawRect( 0, active and 0 or 2, w, h )
			surface.SetDrawColor( S.dark )
			surface.DrawRect( 0, active and 0 or 2, w, 1 )
			surface.DrawRect( 0, active and 0 or 2, 1, h )
			surface.DrawRect( w - 1, active and 0 or 2, 1, h )
			if ( !active ) then surface.DrawRect( 0, h - 1, w, 1 ) end
			Text( name, "OpenGMODSource", active and S.white or S.label, w / 2, h / 2 + ( active and 0 or 1 ), TEXT_ALIGN_CENTER )
		end
		tab.OnMousePressed = function() self:Show( page ) end
		page:SetParent( self )
		page:SetPos( 0, TAB )
		page:SetSize( w, h - TAB )
		page:SetVisible( false )
		self.Tabs[ #self.Tabs + 1 ] = tab
		self.Pages[ #self.Pages + 1 ] = page
		if ( !self.Active ) then self:Show( page ) end
	end
	function sheet:Show( page )
		if ( IsValid( self.Active ) ) then self.Active:SetVisible( false ) end
		self.Active = page
		page:SetVisible( true )
	end
	return sheet
end

------------------------------------------------------------------------------------------------
-- a page from its .res: each control where the file puts it

local SKIP_NAMES = { BuildModeDialog = true, Cancel = true, ok = true, OK = true, Apply = true }

local function Place( panel, control, parent )
	local pw, ph = parent:GetSize()
	local x, xs = Number( control.xpos, 0 )
	local y, ys = Number( control.ypos, 0 )
	if ( xs == "r" ) then x = pw - x elseif ( xs == "c" ) then x = pw / 2 + x end
	if ( ys == "r" ) then y = ph - y elseif ( ys == "c" ) then y = ph / 2 + y end
	panel:SetPos( x, y )
	panel:SetSize( Number( control.wide, 64 ), Number( control.tall, 24 ) )
	panel:SetEnabled( control.enabled != "0" )
	if ( control.tooltiptext and control.tooltiptext != "" ) then panel:SetTooltip( Phrase( control.tooltiptext ) ) end
end

local MAKERS = {}
MAKERS.Label = function( parent, c )
	local label = vgui.Create( "OpenGMODSourceLabel", parent )
	label:SetLabel( Phrase( c.labeltext ), c.wrap == "1" )
	local align = c.textalignment or "west"
	if ( align:find( "north" ) ) then label.AlignY = TEXT_ALIGN_TOP end
	if ( align == "center" or align == "north" or align == "south" ) then label.AlignX = TEXT_ALIGN_CENTER end
	if ( align:find( "east" ) ) then label.AlignX = TEXT_ALIGN_RIGHT end
	return label
end
MAKERS.Button = function( parent, c )
	local button = vgui.Create( "OpenGMODSourceButton", parent )
	button:SetLabel( Phrase( c.labeltext ) )
	return button
end
MAKERS.URLButton = function( parent, c )
	local button = MAKERS.Button( parent, c )
	button.Link = true
	return button
end
MAKERS.CheckButton = function( parent, c )
	local check = vgui.Create( "OpenGMODSourceCheck", parent )
	check:SetLabel( Phrase( c.labeltext ) )
	return check
end
MAKERS.CCvarToggleCheckButton = MAKERS.CheckButton
MAKERS.CCvarNegateCheckButton = MAKERS.CheckButton
MAKERS.ComboBox = function( parent, c ) return vgui.Create( "OpenGMODSourceCombo", parent ) end
MAKERS.CLabeledCommandComboBox = MAKERS.ComboBox
MAKERS.CCvarSlider = function( parent, c )
	local slider = vgui.Create( "OpenGMODSourceSlider", parent )
	slider.LeftText, slider.RightText = Phrase( c.lefttext ), Phrase( c.righttext )
	slider.Cvar = c.cvar_name
	local min, max = tonumber( c.minvalue or "" ), tonumber( c.maxvalue or "" )
	if ( min and max ) then slider:SetRange( min, max ) end
	return slider
end
MAKERS.Slider = MAKERS.CCvarSlider
MAKERS.TextEntry = function( parent, c ) return vgui.Create( "OpenGMODSourceEntry", parent ) end
MAKERS.CCvarTextEntry = MAKERS.TextEntry
MAKERS.Divider = function( parent, c ) return vgui.Create( "OpenGMODSourceDivider", parent ) end
MAKERS.ImagePanel = function( parent, c )
	local image = vgui.Create( "DImage", parent )
	if ( c.image and c.image != "" ) then image:SetImage( "vgui/" .. c.image ) end
	return image
end
MAKERS.Panel = function( parent, c ) return vgui.Create( "Panel", parent ) end
MAKERS.SectionedListPanel = MAKERS.Panel

local function BuildPage( parent, file )
	local controls = OpenGMOD.ReadResource( file ) or {}
	local made = {}
	for _, c in ipairs( controls ) do
		local kind = c.controlname or ""
		local maker = MAKERS[ kind ]
		if ( maker and !SKIP_NAMES[ c.name ] and !c.name:find( "^frame_" ) and c.visible != "0" ) then
			local panel = maker( parent, c )
			Place( panel, c, parent )
			made[ c.name ] = panel
		end
	end
	return made
end

------------------------------------------------------------------------------------------------
-- what each page's controls do, as GameUI joins them

local function JoinCheck( check, cvar, on, off, default )
	if ( !IsValid( check ) ) then return end
	on, off = on or "1", off or "0"
	check:SetChecked( tostring( Get( cvar, default or off ) ) != off and tonumber( Get( cvar, default or off ) ) != tonumber( off ) )
	check.OnChange = function( _, checked ) Set( cvar, checked and on or off ) end
end

local function Format( value, decimals ) return string.format( "%." .. decimals .. "f", value ) end

-- a slider, with the entry that shows its value when there is one
local function JoinSlider( slider, cvar, min, max, default, entry, decimals )
	if ( !IsValid( slider ) ) then return end
	decimals = decimals or 2
	slider:SetRange( min, max )
	slider:SetValue( GetNumber( cvar, default ) )
	local function Show() if ( IsValid( entry ) ) then entry:SetText( Format( slider.Value, decimals ) ) end end
	Show()
	slider.OnValueChanged = function( _, value )
		Set( cvar, Format( value, decimals ) )
		Show()
	end
	if ( IsValid( entry ) ) then
		local function Take()
			local value = tonumber( entry:GetText() )
			if ( !value ) then return Show() end
			slider:SetValue( value )
			Set( cvar, Format( slider.Value, decimals ) )
			Show()
		end
		entry.OnEnter = Take
		entry.OnLoseFocus = Take
	end
end

local function JoinCombo( combo, choices, current, choose )
	if ( !IsValid( combo ) ) then return end
	combo:Clear()
	for _, choice in ipairs( choices ) do combo:AddChoice( Phrase( choice[ 1 ] ), choice[ 2 ] ) end
	if ( !combo:ChooseData( current ) ) then combo.Selected = 1 end
	combo.OnSelect = function( _, data ) choose( data ) end
end

local function SimpleCombo( combo, cvar, choices, default )
	JoinCombo( combo, choices, Get( cvar, default ), function( data ) Set( cvar, data ) end )
end

local PAGES = {}

PAGES.Game = function( c )
	-- the spray: the logos there are
	if ( IsValid( c.SpraypaintList ) ) then
		local logos = {}
		for _, f in ipairs( file.Find( "materials/vgui/logos/*.vtf", "GAME" ) ) do
			local name = string.StripExtension( f )
			logos[ #logos + 1 ] = { name, name }
		end
		local current = string.StripExtension( string.GetFileFromFilename( Get( "cl_logofile", "materials/vgui/logos/spray.vtf" ) ) )
		JoinCombo( c.SpraypaintList, logos, current, function( name )
			Set( "cl_logofile", "materials/vgui/logos/" .. name .. ".vtf" )
			if ( IsValid( c.LogoImage ) ) then c.LogoImage:SetImage( "vgui/logos/UI/" .. name ) end
		end )
		if ( IsValid( c.LogoImage ) ) then
			c.LogoImage:SetImage( "vgui/logos/UI/" .. current )
			-- the spray on white, as GameUI shows it
			local paint = c.LogoImage.Paint
			c.LogoImage.Paint = function( p, w, h )
				surface.SetDrawColor( S.white )
				surface.DrawRect( 0, 0, w, h )
				if ( paint ) then paint( p, w, h ) end
			end
		end
	end
	if ( IsValid( c.ImportSprayImage ) ) then c.ImportSprayImage:SetEnabled( false ) end
	JoinCheck( c.DisableSprays, "cl_playerspraydisable" )
	SimpleCombo( c.DownloadFilterCheck, "cl_downloadfilter", {
		{ "#GameUI_DownloadFilter_ALL", "all" }, { "#GameUI_DownloadFilter_NoSounds", "nosounds" },
		{ "#GameUI_DownloadFilter_MapsOnly", "mapsonly" }, { "#GameUI_DownloadFilter_None", "none" },
	}, "all" )
	JoinCheck( c.HTMLLoadingScreen, "cl_enable_loadingurl", nil, nil, "1" )
	JoinCheck( c.FastSwitchCheckbox, "hud_fastswitch" )
	if ( IsValid( c.FovSliderSlider ) ) then
		c.FovSliderSlider.LeftText, c.FovSliderSlider.RightText = "75.00", "100.00"
		JoinSlider( c.FovSliderSlider, "fov_desired", 75, 100, 75 )
	end
	if ( IsValid( c.CrosshairSettings ) ) then c.CrosshairSettings.DoClick = function() RunConsoleCommand( "crosshair_setup" ) end end
end

PAGES.Mouse = function( c )
	JoinCheck( c.ReverseMouse, "m_pitch", "-0.022", "0.022", "0.022" )
	if ( IsValid( c.ReverseMouse ) ) then
		c.ReverseMouse:SetChecked( GetNumber( "m_pitch", 0.022 ) < 0 )
		c.ReverseMouse.OnChange = function( _, on ) Set( "m_pitch", Format( ( on and -1 or 1 ) * math.abs( GetNumber( "m_pitch", 0.022 ) ), 3 ) ) end
	end
	JoinCheck( c.MouseFilter, "m_filter" )
	JoinCheck( c.MouseRaw, "m_rawinput", nil, nil, "1" )
	JoinSlider( c.Slider, "sensitivity", 1, 20, 3, c.SensitivityLabel )
	JoinSlider( c.MouseAccelerationSlider, "m_customaccel_exponent", 1, 1.4, 1.05, c.MouseAccelerationLabel )
	local function Accel( on )
		for _, name in ipairs( { "MouseAccelerationSlider", "MouseAccelerationLabel" } ) do
			if ( IsValid( c[ name ] ) ) then c[ name ]:SetEnabled( on ) end
		end
	end
	if ( IsValid( c.MouseAccelerationCheckbox ) ) then
		local on = GetNumber( "m_customaccel", 0 ) != 0
		c.MouseAccelerationCheckbox:SetChecked( on )
		Accel( on )
		c.MouseAccelerationCheckbox.OnChange = function( _, checked ) Set( "m_customaccel", checked and "3" or "0" ) Accel( checked ) end
	end
	-- the gamepad's: there is none in openGMOD yet
	JoinCheck( c.Joystick, "joystick" )
	JoinCheck( c.ReverseJoystick, "joy_inverty" )
	JoinCheck( c.JoystickSouthpaw, "joy_movement_stick" )
	JoinSlider( c.JoystickYawSlider, "joy_yawsensitivity", -0.5, -7, -0.5, c.JoystickYawSensitivityLabel )
	JoinSlider( c.JoystickPitchSlider, "joy_pitchsensitivity", 0.5, 7, 0.5, c.JoystickPitchSensitivityLabel )
	local function Pad( on )
		for _, name in ipairs( { "ReverseJoystick", "JoystickSouthpaw", "JoystickYawSlider", "JoystickPitchSlider", "JoystickYawSensitivityPreLabel", "JoystickPitchSensitivityPreLabel" } ) do
			if ( IsValid( c[ name ] ) ) then c[ name ]:SetEnabled( on ) end
		end
	end
	if ( IsValid( c.Joystick ) ) then
		Pad( c.Joystick.Checked )
		c.Joystick.OnChange = function( _, on ) Set( "joystick", on and "1" or "0" ) Pad( on ) end
	end
end

PAGES.Audio = function( c )
	JoinSlider( c.SFXSlider, "volume", 0, 1, 1 )
	JoinSlider( c.GModSFXSlider, "opengmod_sfx_volume", 0, 1, 1 )
	JoinSlider( c.MusicSlider, "snd_musicvolume", 0, 1, 1 )
	SimpleCombo( c.SpeakerSetup, "snd_surround_speakers", {
		{ "#GameUI_Headphones", "0" }, { "#GameUI_2Speakers", "2" }, { "#GameUI_4Speakers", "4" },
		{ "#GameUI_5_1Speakers", "5" }, { "#GameUI_7_1Speakers", "7" },
	}, "2" )
	SimpleCombo( c.SoundQuality, "opengmod_sound_quality", { { "#GameUI_High", "2" }, { "#GameUI_Medium", "1" }, { "#GameUI_Low", "0" } }, "2" )
	local captions = GetNumber( "closecaption", 0 ) == 0 and "0" or GetNumber( "cc_subtitles", 0 ) != 0 and "2" or "1"
	JoinCombo( c.CloseCaptionCheck, {
		{ "#GameUI_NoClosedCaptions", "0" }, { "#GameUI_SubtitlesAndSoundEffects", "1" }, { "#GameUI_Subtitles", "2" },
	}, captions, function( data )
		Set( "closecaption", data == "0" and "0" or "1" )
		Set( "cc_subtitles", data == "2" and "1" or "0" )
	end )
	JoinCombo( c.AudioSpokenLanguage, { { "#GameUI_Language_English", "english" } }, "english", function() end )
	JoinCheck( c.snd_mute_losefocus, "snd_mute_losefocus", nil, nil, "1" )
	JoinCheck( c.dsp_enhance_stereo, "dsp_enhance_stereo" )
	if ( IsValid( c.ThirdPartySoundCredits ) ) then
		c.ThirdPartySoundCredits.DoClick = function() Options.Dialog( "resource/optionssubaudiothirdpartydlg.res", "#GameUI_ThirdPartyAudio_Title" ) end
	end
end

local function AspectOf( w, h )
	local r = w / h
	if ( math.abs( r - 4 / 3 ) < 0.02 or math.abs( r - 5 / 4 ) < 0.02 ) then return "4:3" end
	if ( math.abs( r - 16 / 9 ) < 0.02 ) then return "16:9" end
	if ( math.abs( r - 16 / 10 ) < 0.02 ) then return "16:10" end
	return "other"
end

PAGES.Video = function( c )
	local modes = OpenGMOD.VideoModes() or {}
	local native = modes.native or { 0, 0 }
	local function Resolutions( aspect )
		local choices = {}
		for _, m in ipairs( modes ) do
			if ( aspect == "any" or AspectOf( m[ 1 ], m[ 2 ] ) == aspect ) then
				local text = m[ 1 ] .. " x " .. m[ 2 ]
				if ( m[ 1 ] == native[ 1 ] and m[ 2 ] == native[ 2 ] ) then text = text .. " (native)" end
				choices[ #choices + 1 ] = { text, m[ 1 ] .. "x" .. m[ 2 ] }
			end
		end
		JoinCombo( c.Resolution, choices, Get( "opengmod_width", native[ 1 ] ) .. "x" .. Get( "opengmod_height", native[ 2 ] ), function( data )
			local w, h = data:match( "(%d+)x(%d+)" )
			Set( "opengmod_width", w )
			Set( "opengmod_height", h )
		end )
	end
	Resolutions( "any" )
	JoinCombo( c.AspectRatio, {
		{ "#GameUI_AspectAny", "any" }, { "#GameUI_AspectNormal", "4:3" }, { "#GameUI_AspectWide16x9", "16:9" }, { "#GameUI_AspectWide16x10", "16:10" },
	}, "any", Resolutions )
	SimpleCombo( c.DisplayModeCombo, "opengmod_fullscreen", { { "#GameUI_Fullscreen", "1" }, { "#GameUI_Windowed", "0" }, { "#GameUI_WindowedNoBorder", "2" } }, "0" )
	-- no limit (0) is the most the slider goes to
	if ( GetNumber( "fps_max", 300 ) < 30 ) then Pending.values.fps_max = nil end
	JoinSlider( c.FPSLimitSlider, "fps_max", 30, 300, 300, c.FPSLimitLabel, 0 )
	if ( IsValid( c.FPSLimitSlider ) and GetNumber( "fps_max", 300 ) < 30 ) then
		c.FPSLimitSlider:SetValue( 300 )
		if ( IsValid( c.FPSLimitLabel ) ) then c.FPSLimitLabel:SetText( "300" ) end
	end
	if ( IsValid( c.FPSLimitSlider ) ) then c.FPSLimitSlider.LeftText, c.FPSLimitSlider.RightText = "30.00", "300.00" end
	if ( IsValid( c.AdvancedButton ) ) then c.AdvancedButton.DoClick = function() Options.Advanced() end end
	if ( IsValid( c.ThirdPartyVideoCredits ) ) then
		c.ThirdPartyVideoCredits.DoClick = function() Options.Dialog( "resource/optionssubvideothirdpartydlg.res", "#GameUI_ThirdPartyVideo_Title" ) end
	end
end

PAGES.Voice = function( c )
	JoinCheck( c.voice_modenable, "voice_enable", nil, nil, "1" )
	JoinCheck( c.MicBoost, "opengmod_mic_boost", nil, nil, "1" )
	JoinSlider( c.VoiceReceive, "voice_scale", 0, 1, 1 )
	-- GameUI puts the slider of the microphone's volume where its panel is
	local holder = c[ "#GameUI_MicrophoneVolume" ]
	if ( IsValid( holder ) ) then
		local slider = vgui.Create( "OpenGMODSourceSlider", holder:GetParent() )
		slider:SetPos( holder:GetPos() )
		slider:SetSize( holder:GetWide(), 42 )
		JoinSlider( slider, "opengmod_mic_volume", 0, 1, 1 )
	end
	if ( IsValid( c.MicMeter2 ) ) then c.MicMeter2:SetVisible( false ) end
	-- no voice in openGMOD yet
	for _, name in ipairs( { "TestMicrophone", "SteamVoiceSettings" } ) do
		if ( IsValid( c[ name ] ) ) then c[ name ]:SetEnabled( false ) end
	end
end

PAGES.Legal = function( c )
	local urls = {
		PrivacyPolicy = "https://facepunch.com/legal/privacy",
		TermsOfService = "https://facepunch.com/legal/tos",
		ChromiumCredits = "chrome://credits/",
		ServerGuidelines = "https://facepunch.com/legal/servers",
		ModdingGuidelines = "https://facepunch.com/legal/modding",
	}
	for name, url in pairs( urls ) do
		if ( IsValid( c[ name ] ) ) then c[ name ].DoClick = function() gui.OpenURL( url ) end end
	end
end

------------------------------------------------------------------------------------------------
-- the keyboard: GameUI's list of what the keys do (scripts/kb_act.lst), in sections

local function KeyText( key )
	if ( !key ) then return "" end
	local code = input.GetKeyCode( key )
	return code and code != 0 and input.GetKeyName( code ) or key
end

local function KeyboardPage( page, c )
	local list = c.listpanel_keybindlist
	if ( !IsValid( list ) ) then return end
	-- the actions, and the sections they are in
	local rows = {}
	local entries = OpenGMOD.ReadPairs( "scripts/kb_act.lst" ) or {}
	local i = 1
	while ( i <= #entries ) do
		local command, label = entries[ i ][ 1 ], entries[ i ][ 2 ]
		if ( command == "blank" ) then
			if ( !label:find( "^=+$" ) ) then rows[ #rows + 1 ] = { header = Phrase( label ) } end
		else
			rows[ #rows + 1 ] = { command = command, label = Phrase( label ) }
		end
		i = i + 1
	end
	local function KeyOf( command )
		for key, bound in SortedPairs( Pending.binds ) do
			if ( string.lower( bound ) == string.lower( command ) ) then return key end
		end
	end
	local ROW, HEAD = 18, 22
	local scroll = 0
	local function Height() local h = 0 for _, r in ipairs( rows ) do h = h + ( r.header and HEAD or ROW ) end return h end
	list:SetMouseInputEnabled( true )
	list.Paint = function( p, w, h )
		surface.SetDrawColor( S.white )
		surface.DrawRect( 0, 0, w, h )
		surface.SetDrawColor( S.dark )
		surface.DrawOutlinedRect( 0, 0, w, h )
		local keyx = math.floor( w * 0.6 )
		local y = 1 - scroll
		for index, r in ipairs( rows ) do
			local rh = r.header and HEAD or ROW
			if ( y + rh > 0 and y < h ) then
				if ( r.header ) then
					Text( string.upper( r.header ), "OpenGMODSourceSmall", S.list_header, 6, y + rh / 2 + 1 )
					Text( string.upper( Phrase( "#GameUI_KeyButton" ) ), "OpenGMODSourceSmall", S.list_header, keyx, y + rh / 2 + 1 )
					surface.SetDrawColor( S.list_divider )
					surface.DrawRect( 4, y + rh - 2, w - 8 - 15, 1 )
				else
					local selected = p.Selected == index
					if ( selected ) then
						surface.SetDrawColor( S.selected )
						surface.DrawRect( 1, y, w - 2 - 15, rh )
					end
					local color = selected and S.white or S.entry_text
					Text( r.label, "OpenGMODSource", color, 14, y + rh / 2 )
					local key = p.Waiting == index and "" or KeyText( KeyOf( r.command ) )
					Text( key, "OpenGMODSource", color, keyx, y + rh / 2 )
				end
			end
			y = y + rh
		end
		-- the scroll bar
		local total, bar = Height(), 15
		surface.SetDrawColor( S.track )
		surface.DrawRect( w - bar - 1, 1, bar, h - 2 )
		if ( total > h ) then
			local nob = math.max( 20, ( h - 2 * bar ) * h / total )
			local top = bar + ( h - 2 * bar - nob ) * scroll / ( total - h )
			surface.SetDrawColor( S.nob )
			surface.DrawRect( w - bar, top, bar - 2, nob )
			surface.SetDrawColor( S.dark )
			surface.DrawOutlinedRect( w - bar, top, bar - 2, nob )
		end
		for _, at in ipairs( { 1, h - bar - 1 } ) do
			surface.SetDrawColor( S.scroll_button )
			surface.DrawRect( w - bar - 1, at, bar, bar )
			surface.SetDrawColor( S.dark )
			surface.DrawOutlinedRect( w - bar - 1, at, bar, bar )
		end
		surface.SetDrawColor( S.scroll_arrow )
		local ax = math.floor( w - bar / 2 - 1 )
		for i = 0, 3 do
			surface.DrawRect( ax - i, 6 + i, i * 2 + 1, 1 )
			surface.DrawRect( ax - 3 + i, h - bar + 5 + i, 7 - i * 2, 1 )
		end
	end
	local function Scroll( by ) scroll = math.Clamp( scroll + by, 0, math.max( 0, Height() - list:GetTall() + 2 ) ) end
	list.OnMouseWheeled = function( _, delta ) Scroll( -delta * ROW * 3 ) return true end
	local function RowAt( y )
		local at = 1 - scroll
		for index, r in ipairs( rows ) do
			local rh = r.header and HEAD or ROW
			if ( y >= at and y < at + rh ) then return !r.header and index or nil end
			at = at + rh
		end
	end
	local function Edit( index )
		if ( !index ) then return end
		list.Selected, list.Waiting = index, index
		input.StartKeyTrapping()
	end
	list.OnMousePressed = function( p, code )
		local x, y = p:CursorPos()
		if ( x > p:GetWide() - 16 ) then Scroll( y < p:GetTall() / 2 and -ROW * 3 or ROW * 3 ) return end
		local index = RowAt( y )
		if ( !index ) then return end
		local now = SysTime()
		if ( p.Selected == index and p.LastClick and now - p.LastClick < 0.4 ) then Edit( index ) end
		p.Selected, p.LastClick = index, now
	end
	list.Think = function( p )
		if ( !p.Waiting or input.IsKeyTrapping() ) then return end
		local code = input.CheckKeyTrapping()
		local row = rows[ p.Waiting ]
		p.Waiting = nil
		if ( !code or code == KEY_ESCAPE or !row ) then return end
		local key = string.lower( input.GetKeyName( code ) or "" )
		if ( key == "" ) then return end
		-- the key does this alone; what did this before does nothing
		local old = KeyOf( row.command )
		if ( old ) then Pending.binds[ old ] = nil end
		Pending.binds[ key ] = row.command
		if ( Options.OnChanged ) then Options.OnChanged() end
	end
	if ( IsValid( c.ChangeKeyButton ) ) then c.ChangeKeyButton.DoClick = function() Edit( list.Selected ) end end
	if ( IsValid( c.ClearKeyButton ) ) then
		c.ClearKeyButton.DoClick = function()
			local row = rows[ list.Selected or 0 ]
			if ( !row ) then return end
			for key, command in pairs( table.Copy( Pending.binds ) ) do
				if ( string.lower( command ) == string.lower( row.command ) ) then Pending.binds[ key ] = nil end
			end
			if ( Options.OnChanged ) then Options.OnChanged() end
		end
	end
	if ( IsValid( c.Defaults ) ) then
		c.Defaults.DoClick = function()
			Pending.binds = {}
			for _, pair in ipairs( OpenGMOD.ReadPairs( "scripts/kb_def.lst" ) or {} ) do
				Pending.binds[ string.lower( pair[ 1 ] ) ] = pair[ 2 ]
			end
			if ( Options.OnChanged ) then Options.OnChanged() end
		end
	end
end

------------------------------------------------------------------------------------------------

-- keeps what was changed
local function Apply()
	for name, value in pairs( Pending.values ) do OpenGMOD.SetSetting( name, value ) end
	Pending.values = {}
	local before = {}
	for _, pair in ipairs( OpenGMOD.GetBinds() ) do before[ pair[ 1 ] ] = pair[ 2 ] end
	for key in pairs( before ) do
		if ( Pending.binds[ key ] == nil ) then OpenGMOD.BindKey( key, nil ) end
	end
	for key, command in pairs( Pending.binds ) do
		if ( before[ key ] != command ) then OpenGMOD.BindKey( key, command ) end
	end
end

local function StartPending()
	Pending = { values = {}, binds = {} }
	for _, pair in ipairs( OpenGMOD.GetBinds() ) do Pending.binds[ pair[ 1 ] ] = pair[ 2 ] end
end

-- the bottom buttons of a dialog: OK, Cancel and Apply where GameUI puts them
local function Buttons( frame, w, h, names, actions )
	local x = w - 8 - #names * 80
	local made = {}
	for i, name in ipairs( names ) do
		local button = vgui.Create( "OpenGMODSourceButton", frame )
		button:SetLabel( Phrase( name ) )
		button:SetPos( x + ( i - 1 ) * 80, h - 32 )
		button:SetSize( 72, 24 )
		button.DoClick = actions[ i ]
		made[ i ] = button
	end
	made[ 1 ].Default = true
	return made
end

function Options.Open()
	RegisterAll()
	if ( IsValid( Options.Frame ) ) then Options.Frame:Remove() end
	Fonts()
	StartPending()
	local w, h = 512, 406
	local frame = vgui.Create( "OpenGMODSourceFrame" )
	frame:SetLabel( Phrase( "#GameUI_Options" ) )
	frame:SetSize( w, h )
	frame:Center()
	frame:MakePopup()
	Options.Frame = frame

	local sheet = MakeSheet( frame, 8, 30, w - 16, h - 30 - 40 )
	local pages = {
		{ "Game", "resource/optionssubmultiplayer.res", PAGES.Game },
		{ "#GameUI_Keyboard", "resource/optionssubkeyboard.res", KeyboardPage },
		{ "#GameUI_Mouse", "resource/optionssubmouse.res", PAGES.Mouse },
		{ "#GameUI_Audio", "resource/optionssubaudio.res", PAGES.Audio },
		{ "#GameUI_Video", "resource/optionssubvideo.res", PAGES.Video },
		{ "#GameUI_Voice", "resource/optionssubvoice.res", PAGES.Voice },
		{ "Legal", "resource/optionssubgmodlegal.res", PAGES.Legal },
	}
	for _, p in ipairs( pages ) do
		local page = vgui.Create( "Panel" )
		sheet:AddPage( Phrase( p[ 1 ] ), page )
		local controls = BuildPage( page, p[ 2 ] )
		p[ 3 ]( controls.listpanel_keybindlist and page or controls, controls )
	end
	sheet:Show( sheet.Pages[ 1 ] )

	local buttons = Buttons( frame, w, h, { "#GameUI_OK", "#GameUI_Cancel", "#GameUI_Apply" }, {
		function() Apply() frame:Close() end,
		function() frame:Close() end,
		function() Apply() Options.Apply:SetEnabled( false ) end,
	} )
	Options.Apply = buttons[ 3 ]
	Options.Apply:SetEnabled( false )
	Options.OnChanged = function() if ( IsValid( Options.Apply ) ) then Options.Apply:SetEnabled( true ) end end
end

-- a dialog of its own .res over the options: the advanced video settings, the credits
function Options.Dialog( file, title, wire )
	local controls = OpenGMOD.ReadResource( file ) or {}
	local w, h = 400, 300
	for _, c in ipairs( controls ) do
		if ( c.controlname and c.controlname:find( "Dlg$" ) ) then w, h = Number( c.wide, w ), Number( c.tall, h ) end
	end
	local frame = vgui.Create( "OpenGMODSourceFrame" )
	frame:SetLabel( Phrase( title ) )
	frame:SetSize( w, h )
	frame:Center()
	frame:MakePopup()
	local made = BuildPage( frame, file )
	if ( wire ) then wire( made, frame ) end
	for _, name in ipairs( { "Button1", "Button2", "OK", "Close", "CloseButton" } ) do
		if ( IsValid( made[ name ] ) and !made[ name ].DoClick ) then made[ name ].DoClick = function() frame:Close() end end
	end
	return frame, made
end

function Options.Advanced()
	local saved = table.Copy( Pending.values )
	Options.Dialog( "resource/optionssubvideoadvanceddlg.res", "#GameUI_VideoAdvanced_Title", function( c, frame )
		local function Levels( names )
			local out = {}
			for i, n in ipairs( names ) do out[ i ] = { n[ 1 ], n[ 2 ] } end
			return out
		end
		local low, medium, high = "#gameui_low", "#gameui_medium", "#gameui_high"
		SimpleCombo( c.ModelDetail, "r_rootlod", Levels( { { low, "2" }, { medium, "1" }, { high, "0" } } ), "0" )
		SimpleCombo( c.TextureDetail, "mat_picmip", Levels( { { low, "2" }, { medium, "1" }, { high, "0" }, { "#gameui_ultra", "-1" } } ), "0" )
		SimpleCombo( c.ShaderDetail, "mat_reducefillrate", Levels( { { low, "1" }, { high, "0" } } ), "0" )
		SimpleCombo( c.WaterDetail, "opengmod_water_detail", Levels( { { "#gameui_noreflections", "0" }, { "#gameui_reflectonlyworld", "1" }, { "#gameui_reflectall", "2" } } ), "2" )
		SimpleCombo( c.ShadowDetail, "opengmod_shadow_detail", Levels( { { low, "0" }, { medium, "1" }, { high, "2" } } ), "2" )
		SimpleCombo( c.ColorCorrection, "mat_colorcorrection", Levels( { { "#gameui_disabled", "0" }, { "#gameui_enabled", "1" } } ), "1" )
		SimpleCombo( c.AntialiasingMode, "mat_antialias", Levels( { { "#GameUI_None", "1" }, { "#GameUI_2X", "2" }, { "#GameUI_4X", "4" }, { "#GameUI_8X", "8" } } ), "1" )
		SimpleCombo( c.FilteringMode, "mat_forceaniso", Levels( {
			{ "#GameUI_Bilinear", "0" }, { "#GameUI_Trilinear", "1" }, { "#GameUI_Anisotropic2X", "2" },
			{ "#GameUI_Anisotropic4X", "4" }, { "#GameUI_Anisotropic8X", "8" }, { "#GameUI_Anisotropic16X", "16" },
		} ), "8" )
		SimpleCombo( c.VSync, "mat_vsync", Levels( { { "#gameui_disabled", "0" }, { "#gameui_enabled", "1" } } ), "1" )
		SimpleCombo( c.MotionBlur, "mat_motion_blur_enabled", Levels( { { "#gameui_disabled", "0" }, { "#gameui_enabled", "1" } } ), "0" )
		SimpleCombo( c.Multicore, "mat_queue_mode", Levels( { { "#gameui_disabled", "0" }, { "#gameui_enabled", "-1" } } ), "-1" )
		SimpleCombo( c.HDR, "mat_hdr_level", Levels( { { "#GameUI_hdr_level0", "0" }, { "#GameUI_hdr_level1", "1" }, { "#GameUI_hdr_level2", "2" } } ), "2" )
		SimpleCombo( c.D3D9Ex, "mat_disable_d3d9ex", Levels( { { "#gameui_enabled", "0" }, { "#gameui_disabled", "1" } } ), "0" )
		-- in the DirectX levels' place, what draws: the graphics API now, and the one to draw
		-- with from the next start
		local renderer = OpenGMOD.Renderer()
		local names = { dx12 = "Direct3D 12", vulkan = "Vulkan" }
		if ( IsValid( c.dxinstalledlabel ) ) then c.dxinstalledlabel:SetLabel( names[ renderer.api ] or renderer.api ) end
		local choices = {}
		for _, api in ipairs( renderer.available ) do
			local text = names[ api ] or api
			if ( api != renderer.api ) then text = text .. " " .. Phrase( "#GAMEUI_RequiresRestart" ) end
			choices[ #choices + 1 ] = { text, api }
		end
		local chosen = Get( "opengmod_renderer", "auto" )
		if ( chosen == "auto" ) then chosen = renderer.api end
		JoinCombo( c.dxlabel, choices, chosen, function( api ) Set( "opengmod_renderer", api ) end )
		if ( IsValid( c.dxlabel ) ) then c.dxlabel:SetEnabled( #choices > 1 ) end
		-- OK keeps what was chosen here with the rest; Cancel forgets it
		if ( IsValid( c.Button1 ) ) then c.Button1.DoClick = function() frame:Close() if ( Options.OnChanged ) then Options.OnChanged() end end end
		if ( IsValid( c.Button2 ) ) then c.Button2.DoClick = function() Pending.values = saved frame:Close() end end
	end )
end

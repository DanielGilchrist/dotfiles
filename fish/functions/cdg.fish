function cdg --description "Change directory to a specified gem"
    if test (count $argv) -eq 0
        echo "Usage: cdg GEM_NAME"
        return 1
    end

    set -l gem_name $argv[1]
    set -l ruby_version (asdf current ruby | awk '{print $2}')

    if test -z "$ruby_version"
        echo "No Ruby version found. Make sure asdf and Ruby plugin are properly installed."
        return 1
    end

    set -l gem_path (gem environment home)

    if test -z "$gem_path"
        echo "Could not determine gem path."
        return 1
    end

    set -l gem_dirs (ls -d $gem_path/gems/$gem_name-* 2>/dev/null)

    if test (count $gem_dirs) -eq 0
        echo "Gem '$gem_name' not found."
        return 1
    else if test (count $gem_dirs) -gt 1
        echo "Multiple versions found. Please choose one:"
        for i in (seq (count $gem_dirs))
            echo "  $i: $gem_dirs[$i]"
        end
        read -P "Enter number: " choice
        if test "$choice" -ge 1 -a "$choice" -le (count $gem_dirs)
            cd $gem_dirs[$choice]
        else
            echo "Invalid choice."
            return 1
        end
    else
        cd $gem_dirs[1]
    end
end

